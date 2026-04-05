# Task Graph Schema

The downstream formatter and `submit_plan` tool expect a task graph with:

- top-level `goal`: non-empty string
- top-level `tasks`: list of task objects

Each task object must include:

- `task_id`
- `description`
- `agent_name`
- `expandable`
- `expansion_hint`
- `depends_on`
- `touches_paths` (non-empty list of concrete in-repo owned paths)

Grounding contract:

- Every task must be anchored to concrete in-repo ownership from synthesis with non-empty `touches_paths` and, when useful, `touches_symbols`.
- A preserved explored region is grounded only when the exact checkout-relative path appears in the codebase map. Basename-only mentions such as `root_model.py` do not count as task ownership grounding.
- `goal`, `project_context`, release prose, and named focus-test files are routing context only. They may prioritize or validate a task, but they do not create standalone ownership when synthesis has not mapped the change to concrete present-in-checkout paths or symbols.
- Do not create a dedicated task for a goal or changelog bullet unless synthesis maps it to one active concrete in-repo owned slice, hotspot family, or direct validation surface in this checkout.
- If synthesis says an item is already fixed, already present, or requires no local change, omit a dedicated task unless another synthesized hotspot contradicts that status.
- If synthesis says an upstream, adjacent-repo, dependency-bump, or version-only item requires no local change in this checkout, omit that lane entirely instead of creating a placeholder verification or confirmation task.
- If synthesis, exploration notes, or runtime context explicitly say an item is delegated upstream or needs no local change in this checkout, do not create a local implementation lane anchored only on nearby tests, types, or validation pressure just to keep checklist coverage.
- If a changelog bullet or spec item is not grounded by synthesis, do not append it to a neighboring owned task just to keep checklist coverage. Omit it until synthesis maps that item to the task's concrete owned slice.
- Do not emit a task whose primary scope is only an adjacent repository, upstream project, or release-note heading without mapped in-repo paths or symbols.
- If the release notes mention adjacent-repo work but synthesis does not map it to this checkout, do not create a standalone task for it at this submitted level.
- If synthesis marks a path or surface as absent from the current checkout, do not bundle that absent area into an owned task or `expansion_hint` together with concrete in-repo work.
- Do not mix absent checkout ownership with present owned paths inside one task. If a docs/workflow/config surface is absent or blocked, omit it from `touches_paths` and keep it only as blocked context for the nearest concrete in-repo lane when needed.
- Do not create a standalone root task whose primary owned work is only rerunning named focus tests or cross-release verification commands across several implementation lanes. Keep those validation files attached to the owning implementation slices unless the task owns shared test-harness infrastructure.
- Do not create a standalone implementation task from release prose or a named focus-test file alone when synthesis has not mapped that behavior to a concrete owned slice in this checkout.
- Do not make a dependency or version-bump task block unrelated implementation tasks by default. Add a dependency only when the downstream task concretely needs symbols, behavior, or config introduced by that bump before its owned work can proceed.
- Do not append sibling package/build config files to a dependency or version-bump task unless synthesis or visible checkout context explicitly grounds each file. A lane grounded only on `pyproject.toml` must not claim `setup.py`, `setup.cfg`, or `requirements*.txt` by convention alone.
- Do not bundle a dependency-bump or package/build-config file such as `pyproject.toml`, `setup.py`, `setup.cfg`, or `requirements*.txt` into the same leaf behavior lane as a concrete source file unless that config file itself is the local fix surface.
- If one changelog item mixes an upstream/dependency bump with a local behavior fix, split the local behavior lane from the dependency/config lane or omit the no-local-change bump entirely instead of serializing unrelated work behind that mixed lane.
- At the root submitted level, do not widen back to a broad parent path when synthesis already exposed narrower child slices or hotspot families.
- If synthesis already isolated several file-level hotspots inside one subsystem, preserve them as separate tasks or separate expandable branches instead of wrapping them in one umbrella lane.
- Do not merge changelog items into one leaf when synthesis anchors them to different owned files or clusters, even if they share a validation file, release section, or nearby subsystem label. Keep them separate, or keep the combined lane expandable.
- If two changelog items share one hotspot file but only one item spills into a second owned file or ownership cluster, do not keep that bundle as a leaf. Split the single-file item out, or keep the combined lane expandable.
- `touches_paths` should use concrete present-in-checkout files or small visible path clusters. Do not use broad placeholders like `docs/`, `.github/`, or `dask/dataframe/tests/` when the actual owned files are known, and do not use absent paths as ownership anchors.
- When a task introduces, renames, or retypes a public symbol, treat thin package/barrel export wiring as companion work for the same branch when that wiring is part of the same ownership cluster.
- When synthesis or runtime-visible code already shows the package entrypoint, barrel file, lazy import map, registry, or similar public import surface that must expose that same branch, include those exact files in `touches_paths` instead of relying on prose-only export ownership.
- A leaf is invalid when that visible companion import/export surface is omitted from `touches_paths`, because a correct public-surface edit could then fail workspace validation as out-of-scope.
- If adding that export surface would create multiple primary ownership clusters in an `expandable: false` leaf, keep the task expandable or split into branch-local follow-up work instead of forcing a mixed leaf.
- If validation feedback reports multiple primary ownership clusters for a leaf, revise that lane to one grounded cluster or make it `expandable: true`; do not retry the same mixed leaf shape.
- Do not rely on submit_plan or validator repair to auto-promote a mixed leaf into a coordinator branch. The next draft must already split, omit, or intentionally mark that branch expandable.
- Do not create a standalone thin export-only root task when the owned behavior lives in a sibling implementation branch.
- If synthesis mentions a future symbol only as something to add, introduce, expose, or make importable, that spelling is provisional until a visible symbol or export anchors it.
- Negative existence wording such as `no dedicated X type exists`, `X is not defined`, or `X is missing` still leaves that symbol provisional until a visible symbol or export anchors it.
- If synthesis or runtime-visible code does not confirm the exact public symbol spelling, describe the task by behavior and owned path cluster instead of inventing a class or function name from release prose.
- Do not bake guessed public API symbol names into `task_id`, `description`, or `expansion_hint`.
- Do not freeze provisional future symbol names into `touches_symbols` either.
- Do not emit a leaf task when its real production fix surface is still ambiguous or likely spills into sibling implementation files outside its declared `touches_paths`.
- Do not emit a leaf task when its only concrete anchor comes from an unexplored helper note, investigation-required hotspot, or `exploration_gaps` entry. Keep that lane expandable or attach it to the nearest explored owning branch until the exact fix surface is grounded.
- If validation feedback reports that a path was never grounded by synthesis, revise that lane to exact checkout-relative grounded paths or omit it until synthesis carries the anchor. Do not retry the same basename-only or prose-alias path.
- Do not combine an explored file-owned fix with an investigation-only gap-backed fix inside one leaf. Keep the gap-backed work separate or expandable until its concrete ownership is grounded.
- If synthesis keeps a runtime API entrypoint or consumer surface unresolved while a sibling helper, generator, or internal pipeline file is only a suspected execution surface for the same behavior, do not re-anchor the task solely on that helper and do not collapse both clues into one mixed leaf. Keep one branch that still names the entrypoint/consumer surface until the definitive execution site is grounded.
- Under child planning, treat the parent `expansion_hint` as a branch boundary rather than a literal file whitelist. If synthesis grounds an adjacent sibling execution file, helper, or internal generator inside that same branch as necessary for the same behavior fix, include that ownership in the task or keep the lane expandable.
- Do not emit a leaf task whose `touches_paths` are so narrow that a valid adjacent-file edit inside the same owned branch would fail workspace validation as out-of-scope.
- If the branch is still an interaction bug across multiple production layers or files, such as schema generation, metadata application, validator wrappers, serializer adapters, or config propagation, do not split it into per-file leaves until one concrete execution site is grounded. Keep the slice expandable or emit one broader branch-local task instead.
- If synthesis confidence is low, or synthesis came mostly from partial/truncated exploration and two sibling production files still look plausible for one behavior fix, do not emit separate fixed-file leaves that guess one file each. Keep one broader branch-local task that owns those sibling surfaces, or keep the lane expandable until one definitive execution site is confirmed.
- Do not duplicate one changelog item, runtime behavior bug, or synthesized hotspot across multiple leaves unless each leaf owns a separately grounded independently shippable slice. Supporting helper edits for that same behavior should stay in one branch or keep the lane expandable.
- Do not emit both an expandable branch and a sibling leaf for the same unresolved changelog item or runtime bug at one submitted level. Pick one owner for that behavior until the execution surface is fully grounded.
- At one submitted level, every non-verifier task must own a distinct primary implementation slice. If two sibling tasks have the same primary implementation paths, they are duplicate ownership restatements and one of them must be merged, removed, or split into disjoint owned paths.
- A backup anchor is not a separate task. If one branch already owns the uncertainty, nearby concrete files may stay inside that branch's owned scope, but they must not also appear as a sibling task for the same bug.
- After validator feedback on a mixed leaf or broad branch, replace that invalid lane instead of keeping it and adding a second "remaining", "follow-up", or "cross-cutting" umbrella over the same primary files.
- If synthesized or explored evidence already says the actual behavior executes in a sibling file or method, do not emit a leaf on the wrapper, declaration, or delegating module instead. Anchor the task on the execution file and keep the wrapper file only when it also needs a concrete edit.
- Do not split one propagation bug into separate upstream-caller and downstream-callee leaves when one side may only forward config, kwargs, options, or context into the same behavior. Unless synthesis proves both files need distinct edits, keep that caller/callee chain in one branch or keep the lane expandable.
- If a bug is expressed in terms of a constructor, serializer, validator, loader, schema method, or other runtime API entrypoint, keep that entrypoint or consumer file in the task's owned scope. Do not emit a helper-only leaf on downstream pipeline modules when they only support the same behavior.
- Do not emit a leaf task whose only owned anchor is a thin package entrypoint, barrel export, compat shim, or import surface when the behavior under repair is likely implemented or enforced in sibling production files. Either include that concrete downstream ownership or keep the lane expandable.
- If release prose only mentions runtime import, availability, or re-export behavior and synthesis does not ground a concrete present-in-checkout fix surface, do not create a standalone import or revert leaf from prose alone.
- Task IDs and descriptions should reflect the owned slice itself, not an abstract umbrella lane or catch-all bucket.
- A task that mixes `.github/workflows/` or other CI paths with `setup.py`, `pyproject.toml`, `setup.cfg`, `requirements*.txt`, or docs is presumed over-broad unless those files form one tiny atomic infrastructure patch with the same direct validation surface.
- If runtime context names dominant FAIL_TO_PASS or PASS_TO_PASS focus files, every such file must map to a task's owned scope or to a nearby owned lane with a concrete justification. Do not let named focus files disappear from the graph silently.
- Do not bundle multiple dominant focus-test files from different directories or unrelated validation surfaces into one task unless they clearly belong to one shared validation cluster.
- If one task directly owns three or more dominant focus files, the planner should normally split that lane or explicitly justify why those files are one coherent cluster.
- Do not use one root-level expandable test bucket to collect focus-test files that primarily validate already-explicit implementation lanes. Split those follow-up responsibilities by subsystem or keep them attached to the owning lanes.

Task ID contract:

- keep `task_id` concise, stable, and easy to reference in logs
- prefer short hyphenated IDs and aim for `<= 32` characters
- avoid sentence-like IDs; runtime systems may compose child identifiers from `run_id` and `task_id`

Agent assignment contract:

- for `expandable: true`, `agent_name` must equal `phase_settings.expandable_task_agent_name`
- for `expandable: false`, `agent_name` must not equal `phase_settings.expandable_task_agent_name`
- reserve `phase_settings.expandable_task_agent_name` for coordinator-owned expansion only

Scoped expansion contract:

- if `project_context` contains `## Scoped Expansion`, treat it as binding runtime context
- if `project_context` explicitly requests a single recursive branch, keep at most one expandable branch at that submitted level, including root-level hierarchical plans
- large changelog scope, release breadth, or a single available worker do not justify broad umbrella expandables
- when `remaining_expansion_levels > 0`, expandable task count at that submitted level may be anywhere from `0` to `8`
- when `remaining_expansion_levels == 0`, emit zero expandable tasks at that submitted level
- each expandable task should continue its own owned slice more narrowly than its parent
- do not let a few broad expandable root tasks swallow most actionable sibling slices
- if multiple expandable roots remain, each one must have disjoint concrete ownership and a branch-local `expansion_hint`
- if a root draft still contains `2+` expandable tasks under a single-recursive-branch contract, it is invalid and must be revised before submission
- under child planning, treat the parent `expansion_hint` as the owned slice for this submitted level
- under child planning, the parent `expansion_hint` is a branch boundary rather than a literal file whitelist, so adjacent branch-local execution/helper files may still belong to the same task when synthesis grounds them
- the child task's `expansion_hint` must describe only the next narrower slice, not a fresh set of sibling branches

Optional per-task fields accepted and preserved by the runtime:

- `touches_symbols`

Root-task count contract:

- each submitted level must contain `2-8` tasks
- the `2-8` limit counts all tasks at that submitted level, not only expandable tasks
- if a first draft exceeds `8`, regroup sibling work into `expandable: true` buckets
- do not keep a large changelog plan valid by emitting many atomic root tasks

First-submit preflight contract:

- Before returning the first complete draft, split or re-mark any non-expandable leaf that still spans multiple primary ownership clusters.
- Before returning the first complete draft, split any mixed dependency/config + source leaf instead of expecting validator repair to turn it into a coordinator branch.
- Before returning the first complete draft, remove any sibling branch that only restates another task's same primary implementation paths; every non-verifier task must keep distinct primary ownership at that submitted level.
- Before returning the first complete draft, drop any lane whose only remaining role is upstream verification, dependency-bump confirmation, version-only confirmation, or validation of an item already marked as needing no local change.
- For benchmark, test-driven, or macro graphs, the first complete draft must already include exactly one verifier at that submitted level, and that verifier must `depends_on` every non-verifier task at the same level.
- After any split, drop, merge, or verifier insertion, recount the entire submitted level and regroup again until the total task count is back inside `2-8`.

Atomic vs expandable:

- `expandable: false` only for execution-sized tasks with one primary change surface
- `expandable: true` for multi-surface, multi-assignment, or follow-up buckets
- a narrow implementation, investigation, or test lane with one concrete owned file cluster and one direct validation target must not stay `expandable: true`
- a grounded root lane with one concrete owned file cluster and one direct validation target must not stay `expandable: true` merely because one or two branch-local sibling files may still need inspection
- every expandable task needs a concrete `expansion_hint`
- if an `expansion_hint` names multiple child slices, they must be disjoint and non-duplicative
- `expandable: true` does not justify repo-external placeholder tasks or release-wide validation umbrellas; the task still needs one concrete in-repo owned slice
- `expandable: true` does not justify a release-wide "remaining tests" bucket when the tests mainly mirror already-visible implementation lanes
- `expandable: true` at root should narrow one explored region or hotspot family, not replace several sibling regions with a broad parent-path task
- if one worker can plausibly finish a grounded lane inside one owned slice and the next child plan would only restate implementation steps, keep that lane `expandable: false`
- keep a narrow test refactor, warning update, or expectation move attached to the implementation lane it validates instead of reassigning it to CI/tooling/docs infrastructure
- at depth 2 or deeper, do not return an all-expandable planner frontier once concrete owned execution or validation files are known; a valid child level should usually include at least one worker leaf

Dependency contract:

- use `depends_on` only for real blockers
- maximize safe parallelism
- preserve explicit ownership for dominant evaluation surfaces so verification responsibility is visible in the graph
- do not add a dependency just because two tasks are both broad, both foundational, or both mention a shared helper file; require direct overlap in owned files, symbols, or produced behavior
## No Synthetic Wrapper Above Named Slices

If the parent scope already identifies concrete sibling slices for the current level, submit those slices directly as tasks.

Rules:
- When recursive depth remains, any expandable task must be one of the named sibling slices rather than a synthetic wrapper.
- Do not create an additional wrapper task whose only job is to restate the same partition.
- Every expandable task's `expansion_hint` must narrow from that slice to one deeper owned sub-slice.

Preferred:
- `matmul-dot-plan` expandable
- `histogram-coarsen-explore` leaf
- `apply-along-axis-explore` leaf

Avoid:
- `routines-cluster-decomp` expandable
- `matmul-dot-cluster` leaf
- `histogram-coarsen-cluster` leaf
- `apply-along-axis-cluster` leaf

## Root Levels In Hierarchical Workflows

In a hierarchical workflow, root depth may contain one or more expandable tasks depending on the runtime contract.

Rules:
- If the workflow explicitly requests a single recursive branch, emit at most one expandable root task.
- Otherwise, multiple expandable root tasks are allowed when they own disjoint concrete slices with clear `touches_paths` and branch-local `expansion_hint`s.
- Expandable root count may be anywhere from `0` to `8` unless the runtime contract says otherwise.
- If the explored frontier still contains several concrete in-repo sibling slices, keep them visible as separate leaves instead of folding them into broad umbrellas.
