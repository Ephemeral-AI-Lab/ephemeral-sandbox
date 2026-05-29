# Agent Context Recipes — planner · generator · evaluator

Reference for the three context recipes that build each role's **Row 2 context packet**
(`<context>…</context>`). A recipe assembles an ordered list of XML blocks from
TaskCenter store state; `XmlPromptRenderer` renders them in **packet order** (never
reordered), and the "What's in context" outline (Row 3) mirrors the same blocks.

- **planner** and **generator** below document the **as-built** recipes.
- **evaluator** documents the **approved E4 target** (flat, current-attempt-only). Until
  `agent_initial_messages_restructure_PLAN.md` Unit 1 lands, the live evaluator recipe
  still emits the wrapped, over-scoped form (goal + iteration + failed priors + wrapped
  current attempt) — see that PLAN for the migration.

Source of truth is code; keep this in sync with
`backend/src/task_center/context_engine/recipes/`.

| recipe | roles served | required scope | shape |
|---|---|---|---|
| `planner` | planner | `goal_id`, `iteration_id`, `attempt_id` | goal + iteration frame + failed priors |
| `generator` | executor, verifier | `goal_id`, `attempt_id`, `task_id` | flat: plan_spec + deps + assigned task |
| `evaluator` (E4) | evaluator | `attempt_id` | flat: plan_spec + task evidence + criteria |

Principle: **each recipe carries only what the role must act on.** The planner needs goal
scope to plan; the generator needs only its local task contract; the evaluator needs only
the current attempt it judges against its criteria.

---

## `planner` — `recipes/planner.py`

Serves the **planner**. Built from `goal_iteration_blocks(...)` (`recipes/iterations.py:48-64`)
followed by `failed_attempt_blocks(...)` (`recipes/attempts.py:87-118`).

**Blocks (in order):**
1. `<goal>` — always. The user's original request (`Goal.goal`).
2. `<iteration iteration_no="K" status="prior">` — zero or more (iteration 2+). One per prior
   closed iteration, each wrapping `<accepted_plan>` (`prior.plan_spec`) + `<summary>`
   (`prior.task_summary`).
3. `<iteration iteration_no="N" status="current">` — always. Wraps `<iteration_goal>`
   (`iteration.goal`; the literal `(identical to <goal>)` for iteration 1).
4. `<attempt attempt_no="K" status="prior" verdict="fail">` — zero or more, nested inside the
   current iteration. One per failed prior attempt; body = `<plan_spec>`, `<status_summary>`,
   per-task `<task>`, `<evaluation_criteria>`, `<evaluator_summary>`, `<passed_criteria>` /
   `<failed_criteria>`.

The planner does **not** receive the current attempt block.

**Example (iteration 2, one failed prior attempt):**
```xml
<context>
<goal>
{user's original request}
</goal>
<iteration iteration_no="1" status="prior">
<accepted_plan>
{iteration 1 plan_spec}
</accepted_plan>
<summary>
{iteration 1 task summary}
</summary>
</iteration>
<iteration iteration_no="2" status="current">
<iteration_goal>
{iteration 2 scope}
</iteration_goal>
<attempt attempt_no="1" status="prior" verdict="fail">
<plan_spec>{…}</plan_spec>
<status_summary>{…}</status_summary>
<task id="t1" status="success">{…}</task>
<evaluation_criteria>{…}</evaluation_criteria>
<evaluator_summary>{…}</evaluator_summary>
<failed_criteria>{…}</failed_criteria>
</attempt>
</iteration>
</context>
```

---

## `generator` — `recipes/generator.py`

Serves **executor** and **verifier** (identical context shape; only the role
profile/skill/terminals differ). The reference for a lean, **flat** recipe — no goal or
iteration frame.

**Blocks (in order):**
1. `<plan_spec>` — only when `attempt.plan_spec` is set. The attempt's plan, as framing.
2. `<dependency id="…">` — zero or more. One per upstream task in this task's `needs`; body =
   that dependency's latest summary. (`_dependency_blocks`, `generator.py:101-132`.)
3. `<assigned_task task_id="…">` — always, anchored last. This task's local instruction
   (`task.context_message`) — the generator's concrete obligation.

`<dependency>` is the generator-only concept: a task's **upstream inputs**. (The evaluator
does *not* use it — it judges *all* task outputs, which are `<task>` evidence, not deps.)

**Example (a task with one dependency):**
```xml
<context>
<plan_spec>
{attempt plan_spec — framing}
</plan_spec>
<dependency id="t1">
{summary/artifacts produced by upstream task t1}
</dependency>
<assigned_task task_id="t3">
{the instruction t3's executor/verifier must act on}
</assigned_task>
</context>
```

---

## `evaluator` — `recipes/evaluator.py` (E4 target)

Serves the **evaluator**. **Flat, current-attempt-only**: the evaluator gives a binary
pass/fail of *this* attempt against *its* `<evaluation_criteria>`, so it carries only the
current attempt's substance — framing, evidence, authority — and nothing about goal scope,
iteration scope, prior iterations, or prior failed attempts.

**Blocks (in order):**
1. `<plan_spec>` — always (the evaluator runs only after a plan is submitted). The attempt's
   plan, as framing for interpreting the criteria. Built **fresh** from `attempt.plan_spec`
   (verbatim, generator-style) — *not* via the shared `_render_plan_spec_children`, so it
   carries no `<deferred_goal_for_next_iteration>` child. Priority `HIGH` (framing, droppable
   before the criteria under token budget).
2. `<task id="…" status="…">` — zero or more. One per generator task; body = the generator's
   **latest summary only** (no task_spec). Self-closing `<task .../>` when no summary. This is
   the evidence the criteria are judged against; `status` carries pass/block/fail.
3. `<evaluation_criteria>` — when criteria exist. **The authority** — every entry must pass for
   a success verdict. Highest priority (last dropped under token budget).

The evaluator sees the **same blocks whether the attempt closes or defers the goal** — a
defers-goal attempt looks identical here. Its bounded scope is already encoded in the
criteria; the remainder (`<deferred_goal_for_next_iteration>`) is deliberately withheld.

**Removed vs. the pre-E4 evaluator:** `<goal>`, `<iteration_goal>`, `<iteration status="prior">`
background, failed `<attempt status="prior" verdict="fail">` blocks, the `<iteration>`/`<attempt>`
wrappers, **and `<deferred_goal_for_next_iteration>`**. Rationale and migration: see the PLAN.

**Example (two generator tasks — a closes-goal and a defers-goal attempt look identical here):**
```xml
<context>
<plan_spec>
{attempt plan_spec}
</plan_spec>
<task id="t1" status="success">
{t1 summary — what was produced}
</task>
<task id="t2" status="success">
{t2 summary}
</task>
<evaluation_criteria>
{criterion 1}
{criterion 2}
</evaluation_criteria>
</context>
```

**Why summary-only `<task>` evidence (not task_spec + summary):** task_specs are the
generator/verifier contract, already enforced per-task during the attempt. The evaluator works
one level up — attempt vs. attempt-level criteria — and has its own `read_file`/`shell`/`glob`/
`grep` tools for ground truth. Adding task_specs would push it toward per-task completion
checking, competing with criteria-as-authority. If a summary is too thin to judge a criterion,
the fix is a better criterion or summary, not the spec.
