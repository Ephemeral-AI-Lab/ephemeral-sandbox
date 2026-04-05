---
name: scoped-child-planning
description: Child coordinator planning skill for scoped macro expansion. Use when the project context includes a Scoped Expansion section and the coordinator must decompose only the owned child slice.
---

# Scoped Child Planning

Use this skill when the run is expanding an existing macro task, not planning the root graph.

## Role

- Decompose only the owned child slice.
- Treat the parent `expansion_hint` as a scope and ownership constraint, not as a literal file whitelist.
- Use the runtime tool-surface overlay as the source of truth for the exact helper names available in this run.

## Input Contract

The project context may include a `## Scoped Expansion` section with:

- child task identity
- child discovery budget
- current expansion depth
- remaining expansion levels
- parent expansion hint

Those facts are runtime inputs. Use them directly; do not restate them as policy.

## Child Planning Rules

1. Plan only the child macro. Do not re-plan the full repository or recreate the parent's root graph.
2. Use the parent expansion hint to shape the child task graph and ownership boundaries, not to infer a visibility restriction on the repo.
3. Treat the parent expansion hint as an ownership boundary, not as proof that every public symbol name mentioned there is already correct. If the hint includes ungrounded API names, rewrite the child tasks around the verified behavior and owned file cluster until the exact symbol or import surface is confirmed.
3a. the parent `expansion_hint` is a branch boundary, not a literal file whitelist. If branch-local evidence grounds an adjacent sibling execution file, helper, or internal generator inside the same subsystem slice as necessary for the same behavior fix, child tasks may own that file too. Do not freeze a leaf to the first hinted file when that would make the real fix out-of-scope.
4. Keep discovery proportional to the child budget. Once the owned slice is clear enough to draw the child graph, stop exploring.
5. Prefer concrete child tasks over another coordinator layer. Emit further expandable tasks only when the slice still contains multiple independent deliverables and the remaining depth allows it.
6. Do not spend remaining depth just because it exists. If a child already has one concrete owned production-file cluster, one direct validation target, and one worker can plausibly finish it without reopening sibling ownership, emit it as a non-expandable execution task. Use recursive branches only when the slice would otherwise stay over-broad or still contains multiple independent deliverables. Multiple expandable child tasks are allowed only when they own disjoint concrete slices with clear branch-local `expansion_hint`s.
6a. Do not return a planner-only child frontier once the branch already has concrete owned execution or validation files. At depth 2 or deeper, a valid submitted child level should usually contain at least one non-expandable execution leaf; if every child still needs `task-graph-planner`, the branch is not decomposed enough yet.
6b. A narrow implementation lane, investigation lane, or test lane that already names one concrete owned file cluster and one direct validation target must become a non-expandable worker task. Do not keep "implement X", "investigate whether file A or B owns X", or "add tests for X" as another planner task unless the next level can immediately fan out into 2+ disjoint worker leaves.
7. Do not create a one-child recursive chain. If the submitted level would contain only one meaningful child slice, either keep the current task as the execution unit or emit that one child as `expandable: false`. A child planner must not hand off a single expandable task that merely restates the same owned slice at the next depth.
8. Every child `expansion_hint` must describe only one narrower owned slice. Do not reopen sibling branches outside that task's owned slice.
9. If a behavior bug still lives at a consumer call site while a helper or internal file only supports that fix, do not split those into separate misleading leaves unless each leaf can ship and validate independently. Keep the consumer file with its required helper edits, or keep the slice recursive until the boundary is concrete.
9c. Do not split one propagation bug into separate upstream-caller and downstream-callee leaves when one side may only forward config, kwargs, options, or context into the same behavior. Unless synthesis proves that both files need distinct edits, keep the caller/callee chain in one branch or keep it expandable until the true execution site is confirmed.
9a. If the child slice is an interaction bug across multiple production layers or files, such as schema generation, metadata application, validator wrappers, serializer adapters, or config propagation, do not split it into per-file leaves until one concrete execution site is grounded. Keep the slice recursive or emit one broader branch-local task instead of file-locked leaves.
9b. If synthesis confidence is low or exploration was partial/truncated and two sibling production files remain plausible fix surfaces for the same bug, do not turn that ambiguity into separate fixed-file leaves. Keep one broader child task that owns those sibling surfaces, or keep the branch recursive until one definitive execution site is confirmed.
10. For lifecycle or call-order bugs, the owned file is the call site where the order changes. Do not anchor the child task on the helper-definition file unless that helper file itself must change.
11. When a child task names a validation file, use the exact visible checkout-relative path. Do not invent package-local test paths when the checkout shows a shared `tests/...` layout.
12. At depth 2 or deeper, recurse only when the next level can immediately produce 2+ concrete sibling execution tasks with disjoint ownership. If the next step would still be one broad slice, stop recursing and emit execution-sized leaves now.
12a. A narrow test-only follow-up with one visible test module, one warning expectation file, or one direct validation cluster is already execution-sized. Keep it attached to the implementation lane it validates or emit it as `expandable: false`; do not recurse it into another planner-only layer.
13. Preserve parallelism inside the child slice. Use `depends_on` only for real unlock order.
14. End with the narrowest verification task that proves the child slice is complete.

## Output Contract

- Emit a child task graph that completes the parent's owned slice.
- Keep task descriptions self-contained.
- Call `plan_tasks()` exactly once.
## Named Slice Promotion

When the parent scope already names sibling slices, promote those sibling slices directly instead of creating a wrapper task above them.

Rules:
- Do not create a wrapper task above them just to hold the recursion.
- Make every recursive task itself a real owned slice from the parent hint.
- Narrow each recursive task's next `expansion_hint` to one deeper sub-slice inside that chosen slice.
- Do not create a recursive branch solely to consume remaining depth when the chosen slice is already execution-sized.
- Do not emit one-child recursive chains where a task expands into exactly one further expandable task.
- Do not leave a named slice as `expandable: true` once it already has one concrete execution or validation file cluster and no immediate 2+ worker-leaf fan-out.
- Keep sibling slices execution-sized and non-expandable unless the parent scope or runtime contract explicitly allows multiple expandable branches.
