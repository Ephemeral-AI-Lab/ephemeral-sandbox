---
name: changelog-decompose
description: Changelog decomposition skill for coordinators working on software evolution tasks. Decomposes flat release changelogs into parallel task DAGs using the repo-analysis tools actually available in the run to analyze code structure and infer dependencies.
---

# Changelog Decomposition

You are a coordinator decomposing a **software release changelog** into a parallel task graph. Unlike greenfield decomposition, you are working on an **existing codebase** — the code already exists and you must inspect the real repo before you plan.

This skill is **depth-agnostic** — the same process applies whether you are decomposing a full release changelog or a subset of changes scoped to one module.

---

## Simple Default Architecture

Default to this shape unless the codebase evidence forces something more specific:

1. Decompose the work into a small set of owned implementation tasks or clusters.
2. Add only real dependencies between those tasks.
3. Add one verification task at the end that depends on all implementation work.

For most runs, that means:

- 2-4 implementation tasks at the current planning scope
- 0-1 cross-cutting bridge task when a rename or shared API change is real
- 0-1 residual follow-up macro only if uncovered owned work still remains after folding items into primary lanes or verification
- 1 final verification task

References are support material, not the architecture. Start from the simple graph above and only open extra references when the current planning problem actually needs them.
If the plan already fits `owned implementation lanes -> final verification`, stop there. Do not introduce extra planning layers just because a reference describes an edge case.

---

## The Decomposition Process

The process below is there to help you keep the graph simple and correct. It does not create extra mandatory phases beyond:

1. decompose the owned work
2. add reliable dependencies
3. finish with verification

### Step 0 — Scale Detection and Triage

Count the changelog items. If there are **50 or fewer**, skip to Step 1 — the existing flow handles this.

The 50-item threshold is a heuristic proxy for context budget: 50 items at ~20 tokens each is ~1K tokens for the changelog, leaving ample room for a shallow repo survey and a few concrete file reads. Above 50, the changelog competes with discovery output for context space. Adjust per-project if items are unusually verbose (lower the threshold) or terse (raise it).

If there are **more than 50 items**:

1. Perform one shallow repo-structure survey to get the top-level module layout.
2. Classify each changelog item into a **module cluster** using prefix matching:
   - Match explicit module prefixes in item text (e.g., `array:`, `dataframe:`, `distributed:`)
   - Match subpackage directory names from the workspace structure against item text
   - Items with no clear prefix match → `unclassified` bucket
   This is a mechanical classification, not semantic analysis. Do not attempt to deeply understand each item.
3. For each cluster, note:
   - item count
   - whether any FAIL_TO_PASS tests map to this cluster (by matching test file paths to cluster directory prefixes)
   - the primary directory prefix
4. Build a **coverage ledger** before emitting tasks:
   - assign every parsed changelog item to exactly one planned destination:
     - first-frontier cluster
     - bridge task
     - downstream follow-up macro
     - verification-only check
   - if an item is uncertain, it still needs an explicit ledger destination; "we will think about it later" is not coverage
   - use stable bullet references in the planner narrative (for example `CL-001`, `CL-002`) when helpful, but the core requirement is complete one-to-one assignment
5. Apply root frontier budgeting to the clusters:
   - clusters with FAIL_TO_PASS evidence → first-frontier expandable macros
   - clusters without FAIL_TO_PASS evidence → first try to fold them into neighboring owned lanes or verification
   - emit one downstream residual follow-up macro (for example `secondary-release-followups`) only if residual work still spans 2+ concrete owned surfaces or child clusters after that folding pass
   - cross-cutting items (deprecations, renames spanning clusters) → bridge tasks
   - the `unclassified` bucket → fold into that residual follow-up macro or the nearest related cluster
6. Emit the root graph via `plan_tasks()` with:
   - each first-frontier cluster as an expandable task
   - each implementation task naming at least one owned source root in `touches_paths` or the task description (for example `pkg/module`), not just abstract labels like "core" or "follow-up"
   - `expansion_hint` containing: the cluster's full item list, FAIL_TO_PASS mapping, and primary directory prefix
   - one optional residual follow-up macro only when the coverage ledger proves residual owned work remains after folding
   - bridge tasks for cross-cutting work
   - verification task depending on all

The root graph is incomplete if any parsed changelog item is not represented in the coverage ledger. A compact graph is good; silent uncovered bullets are not.

**Stop condition**: Once the root graph is emitted, do NOT continue exploring the codebase. The root coordinator's job is done. Child coordinators handle all deep analysis within their cluster scope.

Do NOT attempt deep analysis on all clusters at the root level. The root coordinator's job for large changelogs is **triage and routing**, not deep planning. Child coordinators handle deep analysis within their cluster scope.

**Depth budget constraint**: The root triage consumes depth 0. Cluster child coordinators run at depth 1. They may emit expandable subtasks (depth 2) whose children execute at depth 3. Do not design cluster plans that require a third level of coordinator expansion — `_MAX_EXPANSION_DEPTH` of 4 allows depths 0-3 only.

**Input size constraint**: If the raw changelog exceeds 50K tokens (e.g., items have multi-paragraph descriptions), it must be pre-truncated to one-line-per-item before the coordinator processes it. Step 0 triage assumes one-line items.

For the full triage heuristics, cluster sizing targets, and a worked example, see `references/large-changelog-triage.md`.

### Step 1 — Analyze the Codebase (BEFORE planning)

Use the repo-analysis tools actually available in the run to understand the existing code structure:

1. **Do not load references by default** — Start from this `SKILL.md` and the simple default architecture above. Open an extra reference only when the current planning problem clearly needs it:
   - `root-frontier-budgeting.md` for large changelogs or capped-concurrency frontier decisions
   - `macro-expansion-and-atomic-ownership.md` when you are about to emit an expandable task
   - `verification-lane-shaping.md` when explicit FAIL_TO_PASS / PASS_TO_PASS evidence should shape staged verification
   - `withheld-tests-and-production-owned-lanes.md` when hidden test patches are withheld
   - `ci-signals-and-lane-shaping.md` when semantic navigation or CI ownership signals materially affect task boundaries
   - `swe-evo-benchmark.md` only for checked-in benchmark-wide constraints
   Do not load references just because they exist. Do not load instance-specific benchmark guides, leaked evaluator notes, or any prompt material that is not part of the checked-in skill/reference set.
   If the simple graph is already defensible from shallow repo inspection, stay in this file and plan.
2. Perform one **shallow repo-structure survey**. Understand the module/package layout, top-level directories, subpackages, and key files.
3. Inspect **2-3 concrete directories or symbols** with the strongest navigation tools actually available in this run. If semantic or symbol tools are unavailable, use targeted search and entry-point file reads instead.
4. Read the **real production/test entry points** for the failing behaviors before writing task descriptions.
5. If semantic cross-file navigation is available and a symbol may affect multiple files, trace **one owned production symbol** before deciding task boundaries.
6. If hotspot or change-coupling hints are available, use them to identify conflict-prone zones where parallel agents need OCC awareness.

Keep the discovery pass narrow. For small/medium releases, prefer 2-3 leaf directories or symbol clusters and 4-6 concrete file reads instead of repeated broad repo-wide surveys.
Once you can name the primary FAIL_TO_PASS behavior clusters and their owned production files, stop exploring secondary CLI or fixture areas unless direct evidence proves they are required unlockers.
Do not keep rereading the same file just because one expected symbol was not found; if a query or read did not improve lane boundaries, count that as evidence to stop broadening the survey.
Never repeat the same broad repo-root survey once you know the narrower directories. Start broad once, then narrow to one concrete directory prefix or file family at a time.

**Broad result truncation**: If a structure or symbol survey returns too much data, narrow to a subdirectory, file family, or symbol kind. Do not repeat the same broad call — use the partial results you already have.

**Semantic reference tracing**: When semantic cross-file navigation is available and a candidate lane may span multiple files, trace at least one owned production symbol before deciding whether that lane stays atomic, becomes expandable, or needs a bridge task. Skipping this on nontrivial lanes leads to task graphs based on directory names rather than real semantic coupling.

### Step 1.5 — Set a planning budget and a stop condition

Before you drill deeper, set an internal budget:

- one repo-root structure survey
- 2-3 concrete directory or symbol inspections, not repo-wide umbrellas
- 4-6 concrete production/test file reads
- 1-2 owned production symbol traces when semantic navigation is available

Stop and plan as soon as you can answer all three questions:

1. What are the 2-4 highest-value owned implementation slices?
2. Which production files or symbols does each slice own?
3. Which slices are true root-frontier work versus downstream follow-ups?

If you cannot answer those after the budget above, keep the lanes smaller or mark them expandable. Do **not** spend the remaining turns trying to reverse-engineer every failing test.

For large changelogs (50+ items) that went through Step 0 triage, the planning budget at the root level is reduced:
- one repo-structure survey (already done in Step 0)
- 1-2 cluster confirmation inspections, solely to confirm cluster assignment
- 2-4 FAIL_TO_PASS entry-point reads to assign them to the correct cluster
- no broad cross-file tracing — that is deferred to child coordinators

The root triage pass should complete in 5-8 tool calls total, not 15-20.

### Step 2 — Anchor on Failing Behaviors

When FAIL_TO_PASS tests or benchmark-authored test patches exist, use them as the primary decomposition anchors:

- Identify the exact failing tests or assertion contracts
- Map them to the production or fixture symbols they exercise
- Cluster work by shared root cause, not by changelog bullet wording alone

If a changelog bullet appears already implemented or uncertain, do **not** create a standalone implementation lane just to “check” it. Fold that check into final verification or the adjacent lane that would own any required code changes.

### Step 3 — Parse the Changelog

Break the changelog into individual items. Each item typically describes one of:
- A new feature or enhancement (e.g., "add plot markers to DVC files")
- A bug fix (e.g., "fix --dry-run")
- A refactoring or rename (e.g., "rename plot to plots")
- A deprecation or removal (e.g., "remove outs_no_cache keys")
- Infrastructure changes (e.g., "Drop Python 3.5 support")

If the root goal or project context already embeds the release text, treat that payload as the primary changelog source. Do **not** spend discovery turns rediscovering the same release notes from on-disk docs or changelog files unless the prompt explicitly says the embedded text was truncated or incomplete.

Look for **PR/issue numbers** (e.g., `#3807`) and **module prefixes** (e.g., `plots:`, `stage:`, `remote:`) in item text — these indicate scope.

### Step 4 — Map Items to Modules

For each changelog item, determine which module(s) it affects:

1. **Keyword matching**: Module names or subpackage names mentioned in the item text.
2. **Symbol references**: If the item mentions a function/class name, use the strongest available symbol/search tools to find where it lives.
3. **File path patterns**: Use the structure survey results to map keywords to directories.

Build an explicit mapping: `changelog_item → [affected_module_1, affected_module_2, ...]`

This mapping is the raw material for the coverage ledger. Before finalizing the root graph, every parsed item must be assigned to exactly one of:

- primary implementation lane
- cross-cutting bridge lane
- downstream follow-up macro
- verification-only check

### Step 5 — Classify Items by Scope

| Scope | Description | Strategy |
|---|---|---|
| **Module-local** | Affects files within a single module/package | Group with other items in same module |
| **Cross-module** | Affects files across 2+ modules | Bridge task, or split per-module if cleanly partitioned |
| **Cross-cutting** | Touches many files across many modules (renames, API changes) | Single bridge task after upstream modules complete |
| **Infrastructure** | Build config, CI, setup.py, docs | Separate task, usually independent |

### Step 6 — Decide Atomic vs Expandable

- **Atomic**: one worker can own the production or fixture change end-to-end, usually one failure mode and one cohesive file cluster
- **Expandable**: the slice spans multiple independent deliverables, multiple major directories, mixed production-plus-test/fixture work, or multiple independent FAIL_TO_PASS clusters

If you are unsure whether a lane should stay atomic or become expandable, load
`references/decomposition-rubric.md` before emitting the final graph.

Do not flatten a broad slice into one atomic task just because the root changelog is short.
At the root level, a lane that names 4+ concrete files, multiple independent failure clusters, or one module-wide “compatibility sweep” is almost never atomic. Split it or mark it expandable.
Mentioning test files as evidence does **not** make a lane a verification task if the owned deliverable is still a production behavior change.

### Step 6.25 — Prove each lane boundary

Before you keep a lane atomic, be able to state:

- the dominant production file or symbol it owns
- the coupled tests or fixtures it may touch
- why another lane does **not** need to participate mid-task

If you cannot defend those three points in one short paragraph, the lane is too broad for an atomic task.

### Step 6.5 — Budget The Root Frontier

For benchmark runs with capped concurrency, spend the first ready worker slots on the highest-value lanes:

- benchmark-critical FAIL_TO_PASS root causes
- fixture or environment work that is strictly required to unblock those fixes

If a changelog bullet is real but lacks direct FAIL_TO_PASS evidence, read-file evidence, or required-unlocker evidence, do not make it an independent first-frontier root lane. Fold it into a neighboring lane, one downstream expandable macro, or final verification.
If every parsed changelog item can already be assigned to a primary lane, bridge lane, or verification, do **not** emit a downstream follow-up macro.
If you collapse multiple remaining bullets into one lane and they still span multiple directories or mixed code-plus-fixture work, that lane should stay expandable rather than becoming one large atomic bucket.
If discovery only proves one or two FAIL_TO_PASS behavior clusters, root-ready non-verification work should usually stay at that size. In those cases, prefer `2 critical lanes + 1 downstream expandable follow-up macro + verification` over a four-lane first frontier.
If semantic reference tracing times out or returns nothing useful, do not keep fishing across the repo. Treat the slice as higher-uncertainty, keep it narrower, or mark it expandable.

### SWE-EVO lane defaults

When this skill is used for SWE-EVO-style benchmark changelogs:

- Default production Python implementation and expansion lanes to `python-developer-sweevo`.
- Reserve a final verification task in the initial DAG whenever the run has multiple implementation lanes, expandable branches, or benchmark verification requirements.
- Prefer `verifier-sweevo` for that task only when `list_specialist_agents()` returns `verifier-sweevo`. If the active roster exposes a different SWE-EVO/team-matched verifier, use that exact agent name instead. Do not invent a verifier name.
- The verifier is a normal planned task, not an engine injection. The planner includes it in the task graph like any other specialist task. If tests fail, the verifier calls `request_replan()` — the engine delivers that signal to the coordinator, which is then re-invoked to revise the plan. There is no engine-level replan logic; this is entirely planner-driven.
- Keep the verifier team-aligned with the implementation lanes. Do not route SWE-EVO verification to a generic `test-engineer` when a SWE-EVO verifier exists in the active roster.
- Prefer one downstream expandable follow-up macro (for example `secondary-release-followups`) over multiple speculative first-frontier chores only when the coverage ledger still contains residual owned work after folding into primary lanes and verification.
- If the residual set is empty, omit the follow-up macro entirely.
- Hidden FAIL_TO_PASS test patches are symptom locators, not deliverables. Every implementation lane must still own at least one production file.

### Step 7 — Build the Task DAG

1. **Group work by shared root cause or module affinity** into owned lanes.
2. **Parallel by default**: independent lanes run in parallel.
3. **Sequential when coupled by real output**: add `depends_on` only when one lane needs another lane's artifact or interface.
4. **Cross-cutting tasks**: create bridge tasks for renames/API changes that depend on the module tasks they span.
5. **Verification task**: one final task that depends on all implementation tasks and runs the project test suite.
6. Every implementation lane must expose a concrete owned repo surface. Prefer `touches_paths`; otherwise cite the source directory or file directly in the description.
7. Before finalizing the DAG, do a coverage pass: confirm every parsed changelog item is assigned to exactly one lane or verification bucket. If any item is unassigned, the plan is not done.

If the planning context includes FAIL_TO_PASS / PASS_TO_PASS evidence, shape the
verification lane as a staged sequence:

1. targeted FAIL_TO_PASS regressions first
2. PASS_TO_PASS guardrails second
3. broader suite or project-wide verification last

Do not describe verification as a single undifferentiated full-suite run when
the benchmark context already gives targeted failing and guardrail surfaces.

---

## Task Graph Shape

Target this structure:

```
[module-A-changes] ──┐
[module-B-changes] ──┤
[module-C-changes] ──┼── [cross-cutting-rename] ── [verification]
[module-D-changes] ──┤
[infra-changes]   ───┘
```

- **2-4 implementation tasks at the root** for small/medium release bumps
- Use **expandable tasks** when a root slice would otherwise hide multiple independent deliverables
- **0-2 cross-cutting bridge tasks** (sequential after upstream modules)
- **1 verification task** (depends on all, runs test suite)

### When to Make Tasks Expandable

- **Expandable** (`expandable: true`): Module cluster has 4+ changelog items OR spans 10+ files. The child coordinator will decompose into atomic subtasks.
- **Atomic** (`expandable: false`): Module cluster has 1-3 simple items affecting <10 files. One agent can complete end-to-end.

Also make a task expandable when it mixes production code with broad fixture/test infrastructure or contains multiple independent failure clusters.

---

## Cross-Cutting Changes

Cross-cutting changes (e.g., "rename `plot` to `plots`" across 20 files) are the hardest to parallelize. Two strategies:

### Strategy A: Bridge Task (Default — Safer)

Create a single bridge task that runs after all affected module tasks complete. One agent performs the rename/refactor across all files.

```
depends_on: [module-A, module-B, module-C]
description: "Rename plot to plots across all modules (20 files)"
expandable: false
```

### Strategy B: Distributed with OCC (Advanced — Leverages Parallelism)

Each module task handles its own portion of the rename. The OCC layer merges concurrent edits to shared files (e.g., `__init__.py`, shared config).

Use Strategy B only when:
- The rename is cleanly partitioned by module (each module's files are distinct)
- Shared files have non-overlapping edit regions
- You set `touches_paths` accurately on each task

---

## Dependency Rules

Same principles as task-decompose, adapted for evolution:

1. **Add `depends_on` only for real data dependencies** — one task produces something another task consumes.
2. **Do NOT add deps for "might edit the same file"** — OCC handles concurrent writes.
3. **DO add deps when**:
   - Task B imports a symbol that Task A is renaming/moving
   - Task B modifies a function signature that Task A is also changing
   - Task B's changes require Task A's new API to be in place
4. **Cross-cutting tasks depend on the modules they span.**
5. **Verification depends on all implementation tasks.**

---

## Parallelism Targets

- **Max chain depth: 3** — `module-changes → bridge-task → verification`
- **Fan-out at failure-cluster or module level** — separate packages/directories are independent when they do not share a root cause
- **Collapse trivially sequential items** — two items in the same file that one agent should handle together
- **Avoid provider over-fan-out** — for small/medium benchmark bumps, root plans should usually stay at 2-4 implementation lanes unless expandable macros are clearly needed
- **Keep the first frontier small** — if effective concurrency is 2, first-frontier root work should usually be no more than 2 benchmark-critical implementation lanes

---

## Writing Task Descriptions for Workers

Each task description should include:

1. **The specific changelog items** this task covers (copy them verbatim)
2. **The affected files/modules** (from your repo analysis)
3. **Key symbols to modify** (function/class names from symbol inspection, semantic navigation, or concrete file reads)
4. **Dependencies context** — what upstream tasks produce that this task uses

Example:
```
Implement the following changelog items in the dvc/stage/ module:

- stage: fix commit (#3816)
- stage: fix --dry-run (#3799)
- stage: moving things around, refactor (#3793)
- stage: hide unwanted warnings (#3763)

Affected files: dvc/stage/__init__.py, dvc/stage/cache.py, dvc/stage/run.py,
dvc/stage/loader.py, dvc/stage/utils.py, dvc/stage/exceptions.py

Key symbols: Stage, PipelineStage, StageLoader, run_stage
```

---

## Self-Check Before Emitting

1. Did I load the generic planning skills and perform a bounded repo survey BEFORE planning?
2. Did I read the real production/test entry points for the failing behaviors?
3. Did I stop exploring once I could name 2-4 owned implementation slices?
4. Does every changelog item appear in exactly one task or explicit verification check?
5. Are there any standalone “check whether it already works” lanes that should be folded away?
6. Is any root atomic lane really a disguised multi-file compatibility bucket?
7. Are module-scoped tasks truly independent (no shared files in `touches_paths`)?
8. Are cross-cutting changes handled as bridge tasks with correct dependencies?
9. Is there a verification task that depends on all implementation tasks?
10. Are expandable tasks genuinely too large for one agent?
11. Does the task count stay within 2-8 at this level?
12. Would any first-frontier lane consume a scarce worker slot without directly advancing FAIL_TO_PASS or a required unlocker?
13. If semantic cross-file navigation was available, did I trace at least one cross-file production symbol before finalizing a nontrivial lane?

---

## Output Contract

Call `plan_tasks()` exactly once with the full task graph.

- Do not wait to be re-invoked — you run exactly once per decomposition scope.
- The engine automatically spawns child coordinators for expandable tasks and ephemeral specialists for atomic tasks.
- Never assign tasks to yourself or any coordinator.
- Never ask clarifying questions.

---

## Optional Reference Guides

These references refine edge cases. They do not replace the simple default architecture.

- **Existing repo release-fix patterns** → `references/existing-repo-release-fixes.md`
- **SWE-EVO benchmark planning** → `references/swe-evo-benchmark.md`
- **Root frontier budgeting** → `references/root-frontier-budgeting.md`
- **Discovery signals and lane shaping** → `references/ci-signals-and-lane-shaping.md`
- **Withheld tests and production-owned lanes** → `references/withheld-tests-and-production-owned-lanes.md`
- **Verification lane shaping** → `references/verification-lane-shaping.md`
- **Large-changelog triage** → `references/large-changelog-triage.md`
- **Macro expansion and atomic ownership** → `references/macro-expansion-and-atomic-ownership.md`
