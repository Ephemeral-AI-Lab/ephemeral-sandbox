# SWE-EVO Benchmark — Decomposition Reference

Use when planning SWE-EVO or SWE-bench-style release-evolution tasks with FAIL_TO_PASS and PASS_TO_PASS test sets.

---

## Success Contract

A plan is only good if it maximizes the chance of:

- all FAIL_TO_PASS tests flipping green
- zero PASS_TO_PASS regressions
- enough grounded repo analysis to keep navigation anchored in the real codebase

That means the plan should start from failing behaviors, not from changelog prose alone.

---

## Repo-First Discovery Sequence

1. Load the generic planning skills plus the SWE-EVO benchmark references.
2. Perform one shallow repo-structure survey to locate the relevant module families.
3. Inspect concrete directories or symbols with the strongest navigation tools actually available in the run.
4. Read the real production or test entry points for the failing behaviors.
5. If semantic cross-file navigation is available and a symbol may have broad impact, trace one owned production symbol before finalizing the lane.
6. Use hotspot or change-coupling hints only when they are actually available.

This sequence improves planning quality without depending on any one CI-query helper name.

---

## Lane Construction Rules

### Good SWE-EVO lanes

- One lane per failing behavior cluster
- One lane for required fixture/test infrastructure only if it is necessary to unlock or validate real fixes
- One final verification lane

### Bad SWE-EVO lanes

- One lane per changelog bullet when two bullets collapse into the same root cause
- A lane that says “verify whether cli help is already implemented”
- A lane that owns only PASS_TO_PASS chores with no required code surface
- A broad “tests” lane that mixes unrelated fixture, regression, and production follow-up work

---

## Root-Level Sizing

For small or medium SWE-EVO instances, prefer **2-4 implementation lanes** at the root.

Why:

- too few lanes hides independent work
- too many lanes causes shallow, noisy graphs and increases worker/provider contention

If more than four real slices exist, use expandable macros and push detail down one level instead of flattening everything at the root.

---

## Large Instances (50+ changelog items)

For large release bumps (e.g., dask 2024.1.0→2024.1.1 with 2000+ items):

- The root coordinator performs prefix-based triage, not deep planning
- Root graph shape: 3-8 expandable cluster macros + bridge + verification
- Each cluster's child coordinator runs the full bounded discovery process within its scope
- FAIL_TO_PASS mapping happens at root level (by matching test file paths to cluster directory prefixes) to ensure correct cluster assignment
- Context budget: root coordinator uses ~40K tokens for the changelog + ~10K for skill refs + ~5K for workspace structure and tool results
- Depth budget: root (0) → cluster child (1) → lane expansion (2) → atomic work (3). No deeper nesting.

See `references/large-changelog-triage.md` for classification heuristics, cluster sizing, and a worked example.

---

## Expandable Triggers

Mark a task `expandable=true` when it contains:

- multiple independent FAIL_TO_PASS clusters
- mixed production code plus broad fixture/test scaffolding
- more than one major directory family
- a natural wave structure such as `shared fixture -> per-backend updates -> regression coverage`

Keep a task atomic when it is a surgical production fix with coupled tests.

---

## Benchmark-Specific Anti-Regression Rule

When a changelog bullet appears already implemented or only partially relevant:

- do **not** create a dedicated “verification” implementation lane
- fold that check into the final verification task
- or attach it to the neighboring lane that owns the code most likely to change

Standalone “maybe no-op” lanes waste one worker slot and lower success rate under provider limits.
