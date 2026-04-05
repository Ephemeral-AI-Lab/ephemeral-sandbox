# Discovery Signals And Lane Shaping — Planning Reference

Use this reference when the run exposes structural survey, semantic navigation, or hotspot signals and you need to turn those signals into better task boundaries without hard-coding removed helper names.

---

## What Good Discovery Usage Looks Like

A strong planning trace usually includes:

1. one repo-root structure survey
2. concrete directory or symbol inspection only
3. reads of the real production and test entry points
4. at least one cross-file trace on an owned production symbol when semantic navigation is available for a nontrivial lane
5. hotspot or coupling hints before final parallelization, if those hints are available

If semantic navigation is available but the planner never performs a cross-file trace on a nontrivial lane, the task graph is probably relying too much on directory names and not enough on semantic coupling.

Set a planning budget before you start:

- one repo-root structure survey
- 2-3 narrow directory or symbol inspections
- 4-6 concrete file reads
- 1-2 cross-file traces for owned production symbols when semantic navigation is available

When that budget is enough to name 2-4 owned implementation slices, stop exploring and plan the graph.

---

## How To Adapt To Available Tooling

### Structural survey + semantic navigation available

- prefer narrow directory or symbol inspection over repeated broad file reads
- use cross-file tracing to decide whether a lane stays atomic or becomes expandable
- treat semantic navigation as the blast-radius check before creating bridge or multi-file lanes

### Structural survey available, semantic navigation partial or unavailable

- keep lanes smaller
- rely on concrete file reads and directory ownership
- avoid broad cross-file refactors unless the file evidence is already strong

### Only search and file reads available

- do not keep attempting unavailable semantic tools
- read a few concrete files, then plan conservatively

---

## Using Semantic Navigation To Shape Lanes

When semantic navigation is available, trace a production symbol for:

- symbols that appear in FAIL_TO_PASS tests and may fan out across files
- candidate bridge symbols such as parser helpers, shared commands, registry functions, or fixture factories
- anything that would otherwise become a multi-directory atomic lane

Interpret the result like this:

- references stay within one cohesive directory cluster → lane can remain atomic
- references span multiple packages but still represent one deliverable → consider one expandable macro
- references touch multiple already-planned lanes → create a downstream bridge task

Avoid counting diagnostics-only usage as sufficient semantic grounding. Diagnostics are useful, but they do not establish blast radius.
If the reference trace times out, returns no symbol, or the same file reads are being repeated, stop broadening the search. Keep the lane narrower or mark it expandable instead of burning turns on more discovery.

---

## Turning CI Signals Into `touches_*`

Populate `touches_paths` from:

- the concrete production/test files you actually read
- hotspot files that the lane will intentionally edit

Populate `touches_symbols` from:

- the owned production symbol you confirmed during symbol inspection or file reading
- the bridge symbol or fixture factory traced during cross-file navigation

Do not leave these empty when the planner already discovered the real files and symbols.

---

## Expandable Triggers From CI Evidence

Prefer `expandable=true` when discovery shows any of:

- one lane spans more than one primary directory family
- reference tracing fans a symbol into multiple packages
- the lane mixes behavior code with broad fixture or environment setup
- the hotspot list includes shared wiring files plus multiple behavior files
- the lane already names 4+ concrete files at the root level
- the lane is described as a module-wide compatibility sweep rather than one dominant behavior change

Keep the lane atomic when:

- one production symbol owns the bug
- reference tracing stays within one file cluster
- the lane can include its coupled tests without hiding unrelated work

Do not classify a lane as verification-only just because its description cites FAIL_TO_PASS test files. Test paths are often evidence for a production-owned implementation lane.

## Root-Lane Anti-Patterns

Reject or reshape these patterns:

1. **Broad compatibility bucket**
   - Example shape: "Fix numpy 2.0 compatibility in `dask.array`" while naming 5 production files
   - Better shape: one lane per owned behavior cluster, or one expandable macro if the cluster still spans independent deliverables

2. **Exploration spiral**
   - Re-reading the same source file multiple times to hunt for one expected symbol
   - Better shape: accept partial evidence, keep the lane smaller, and move to planning

3. **False verification lane**
   - A task is treated as verification because it mentions test files, even though it still owns a production fix
   - Better shape: keep the task implementation-owned and let the final verification lane depend on it
