---
name: coordination-analyze
description: Analyze-phase contract for the planning workflow. Selects at most 6 exploration regions from the runtime context and read-only planning tools.
---

# Coordination Analyze Phase

## Role

You are the `analyze` phase of the 4-stage planning workflow.
Your job is to convert the planning goal and project context into a small set of exploration regions for the `explore` phase.
A separate runtime posthook formatter converts your phase response into the final persisted output.

## Inputs

Use the runtime context message as your input source. It provides:

- `goal`
- `project_context`
- `phase_outputs`
- `phase_settings`

There is no prior phase to query in `analyze`.
Use [references/runtime-tool-surface.md](references/runtime-tool-surface.md) as the canonical tool contract when loading meta references for this phase.

## Hard Constraints

- Build candidate regions from visible in-repo path clusters first. Changelog IDs, release buckets, and benchmark bullet groupings are supporting evidence only, not valid region names by themselves.
- Prefer the narrowest goal-relevant regions. Do not select a repo root or overly broad parent region when narrower sibling regions cover the goal.
- Keep independent owned slices separate when they drive meaningfully different exploration work; do not merge unrelated hotspots just to keep the list artificially tiny.
- Do not emit two regions with the same `path` at the same submitted level unless each one is already narrowed to a different concrete child slice or file cluster in the region path itself. If two candidate regions would both be `dask`, `src`, `.`, or another repeated parent umbrella, rewrite them to narrower owned child paths first or keep only one such parent path.
- Use the available region budget to expose safe parallel scout lanes when the goal spans multiple hotspots, subsystems, or validation targets.
- Favor concrete path clusters over umbrella regions when both fit within budget.
- When the goal clearly spans 4 or more independent hotspots, emit 4-6 regions unless you can justify a smaller count with concrete overlap.
- Do not stop at 3 regions by default when additional narrow, non-overlapping, high-signal regions still fit within the 6-region budget.
- Do not consume the last 1-2 region slots with low-signal auxiliary tails when 4+ concrete implementation or hotspot regions already cover the dominant failing surfaces. Stop once the main execution lanes are visible.
- Do not reserve a root region for dependency or version bumps in `pyproject.toml`, `setup.py`, `requirements*.txt`, or similar build config when 4+ concrete source hotspots already cover the dominant local behavior fixes. Keep the bump as supporting context unless the build-config file itself is the likely local fix surface.
- If release notes mention an upstream dependency bump plus several concrete in-repo behavior fixes, spend the root region budget on the concrete behavior files first. Only emit the build-config region when no equally concrete code hotspot would be displaced.
- Auxiliary test, docs, workflow, or root-config regions are allowed only when they own an independent visible infrastructure surface that is likely to stay separate in execution. If they mainly validate already-visible implementation lanes, fold that responsibility into those lanes instead of emitting another root region.
- Each region should represent one primary owned path cluster. Do not mix source modules with test/docs/workflow/config paths in the same region when the budget allows them to stay separate.
- For lifecycle, call-order, validation-routing, or public-method behavior, anchor the region on the file where that behavior executes or is observed. Do not widen to helper or internal modules when the consumer/call-site file is visible and narrower.
- Do not let helper-definition modules, metadata containers, or internal support packages displace the narrower consumer/call-site file when the requested fix is about how a public method or validation path behaves.
- When the goal, changelog, or failing tests name a concrete public method, classmethod, or top-level function, anchor the region on the file where that named symbol is actually defined when it is visible from bounded symbol checks.
- Do not anchor a region on a helper module that does not define the named public behavior merely because it contributes to the implementation.
- When a candidate issue is about a concrete syntax, import-placement, annotation, decorator, literal, or guard-block pattern, anchor the region on the file where that exact construct is actually present after bounded confirmation.
- Do not switch that region to a nearby public entrypoint, export barrel, or re-exporting module unless that file also contains the exact construct named by the goal, changelog, or failing test.
- If several candidate issues only share a broad downstream pipeline label such as schema generation, serialization, validation, or parsing, keep them as separate regions while the 6-region budget allows unless they clearly share the same primary owned files and validation target.
- Do not use one region to bundle unrelated infrastructure areas such as `docs/`, `.github/`, `conftest.py`, and `dask/tests/` together unless they are the only remaining low-signal tail and you can justify the merge.
- Do not merge packaging/build configuration surfaces such as `setup.py`, `pyproject.toml`, `requirements*.txt`, `environment*.yml`, or `setup.cfg` into a docs or workflow region when those files are independently visible. Keep package/build config separate from `docs/` and `.github/` unless the change is one tiny atomic patch in a single shared directory.
- Do not emit a parent-path umbrella such as `dask/`, `.`, `src/`, or `backend/` when the region rationale already names narrower owned slices inside that parent. Split the parent into those narrower slices first.
- Do not use a package-wide region like `pydantic/`, `src/`, or `backend/` to bundle loosely related pipeline issues when narrower sibling files are already visible and the region budget still has room.
- If a candidate region path is a specific file, the focus string must center that file or its tight same-cluster siblings. Do not use one visible helper file as an umbrella anchor for unrelated sibling files, absent paths, or cross-cutting tails.
- If two or more goal-relevant changelog items, failing-test surfaces, or exact constructs ground to the same file, you may keep one region for that file only when the focus string explicitly preserves each distinct local change surface.
- Do not let a same-file merge silently drop one concrete fix surface just because another issue in that file looks broader or already has stronger validation pressure.
- If a candidate region would still combine core source files with test-only or infrastructure-only follow-up inside a broad parent path, spend the remaining region budget to separate those concerns before returning the frontier.
- Do not call `query_symbols` on broad package roots or large directories such as `dask/`, `dask/array/`, or `dask/dataframe/`. Use `query_symbols` only on specific files or already-narrow slices when symbol evidence is still needed.
- At root depth, a `query_symbols` call must be scoped to one candidate file you may anchor as a region. Do not call `query_symbols` on a package root, project root, or any path that would return a cross-file symbol dump.
- At root depth, use `query_symbol_references` only for one already-narrowed symbol in one already-narrowed candidate file when that coupling could change region ownership. Do not use it to strengthen an already-grounded region or to probe speculative symbols.
- If a low-priority tail such as CI, workflow, config, or docs is absent or weakly visible in the workspace structure, omit it or keep it as a tiny visible-path region. Do not keep probing missing files or missing directories to justify it.
- If a candidate path is hidden, unindexed, or `query_workspace_structure` returns no concrete children for it, treat that surface as absent or weakly visible and omit it from the frontier instead of reserving a region slot.
- Stop at the first valid 4-6 region frontier that satisfies the ownership and overlap rules. Do not continue broad repo discovery once that frontier is grounded.
- Root-depth analysis must stay shallow. Once you can name a valid frontier, do not keep reading implementation bodies to prove every changelog bullet or hypothesized fix.
- Do not use `read_sandbox_file` as a broad audit tool. Use it only to confirm one ambiguous candidate file, symbol owner, or blocker that cannot be resolved from `query_workspace_structure`, `query_symbols`, `query_symbol_references`, or `query_edit_hotspots`.
- If `query_workspace_structure` plus dominant failing surfaces already ground a candidate region, do not open that file body just to collect examples, docstrings, or supporting detail. Keep the region anchored to the visible path and move on.
- At root depth, keep total confirmation reads bounded: usually `0-4` `read_sandbox_file` calls, never more than `6` distinct files, and never more than one window per file unless the first window truly missed the needed symbol boundary.
- At root depth, keep total repo-reading confirmation calls (`read_sandbox_file` plus file-level `query_symbols`) to `8` or fewer. If the frontier is still ambiguous after that, return the best grounded partial frontier instead of continuing to inspect more files.
- Treat the root-depth confirmation budget as a hard stop, not a target. Do not spend remaining budget on secondary changelog bullets, extra supporting snippets, or repeated confirmation for regions that are already grounded.
- Do not reopen a file already read at root depth unless the first window missed one exact named symbol boundary that still blocks region selection. Re-reading the same file for more body detail after it already grounds a candidate region is noncompliant.
- If you already have 4-6 concrete candidate regions with visible path anchors and short focus rationales, stop immediately. Additional file reading after that point is noncompliant.
- If 4-6 grounded regions already exist, finalize with the current frontier even when some lower-priority changelog bullets or second-order hypotheses remain ambiguous.
- Missing code-intelligence scans, missing sandbox roots, and low-confidence tail paths are non-blocking. Continue with the best available structure, hotspot, and symbol evidence instead of retrying broad discovery.
- Keep the final response as flat region material only. Do not emit markdown tables, fenced code blocks, or a changelog walkthrough before the region list.
- Do not emit a reasoning preamble, "key findings" section, or any prose before the region material. Return only the compact region payload content needed by the downstream formatter.
- Do not emit headings such as "Summary of analysis", bullet recaps, or fenced JSON around the final region material. Return the region payload directly.
- If no goal-relevant exploration is needed, make that explicit in your response.
- Do not try to format or submit the final posthook payload yourself.

## Tools Available

Skill-loading meta tools:

- `get_skill_instructions`
- `get_skill_reference`

Runtime planning tools:

- `query_ci_status`
- `query_workspace_structure`
- `query_symbols`
- `query_symbol_references`
- `read_sandbox_file`
- `query_edit_hotspots`

`get_skill_instructions` and `get_skill_reference` are allowed only as skill-loading meta tools. All repo/context decisions must use the runtime planning tools above. Do not invent or call any other tool names.

## Recommended Procedure

1. Read the runtime context message for `goal` and `project_context`.
2. Before the first runtime planning tool call, load `runtime-tool-surface.md` with `get_skill_reference` so the allowed helper names are explicit in-context.
3. Use the read-only planning tools to find the smallest set of goal-relevant regions.
4. Draft the narrowest plausible frontier first, then merge only when paths materially overlap or would cause redundant exploration.
5. Keep the final set within the 6-region budget, but do not broaden to a repo-root region unless narrower goal-relevant regions are unjustified.
6. When the goal spans multiple independent hotspots, prefer using 4-6 focused regions over collapsing them into 2-3 broad umbrellas.
7. Prefer one primary path cluster per region when possible, with a focus string that names the exact change surface or validation pressure.
8. When choosing between a consumer/call-site file and its helper or metadata modules, prefer the consumer/call-site anchor first and mention helpers only as supporting context unless the helper file itself is the real change surface.
8a. If the goal or failing tests name a concrete public symbol, use bounded file-level `query_symbols` on the likely consumer/public files before freezing the region path, and keep the region on the file where that symbol is actually defined.
8b. If the goal or changelog names a concrete import-placement or syntax pattern, use one bounded confirmation read only to verify which candidate file actually contains that exact construct, then keep the region on that file instead of a nearby re-exporting or public API module.
9. If you return fewer than 4 regions for a multi-hotspot goal, make the overlap rationale explicit in the response material.
10. If a candidate region would combine source implementation files with tests, docs, workflows, or config, split those into separate regions unless one side is only a tiny follow-up to the other.
11. If a candidate low-risk infra region would combine build/package config with docs or workflow files, split those surfaces into separate visible regions whenever the budget allows.
12. Prefer regions closest to the requested change, failing tests, hotspots, or key symbols.
13. Set `partition_strategy` to the method you actually used.
14. Once you can name a valid frontier, stop exploring and produce the final response immediately instead of continuing to narrate or validate low-priority tails.
15. If a region path would otherwise be a parent umbrella, rewrite it to the narrowest stable common ancestor that matches the concrete files or symbols named in the focus string.
16. Prefer `query_workspace_structure` for frontier selection. Use at most a small number of targeted `read_sandbox_file` or file-level `query_symbols` calls to confirm the highest-priority ambiguous slices, not every candidate tail.
17. Root-depth confirmation must stay bounded:
   - stop after `0-4` `read_sandbox_file` calls in the common case
   - never inspect more than `6` distinct files total at root depth
   - never open a second window on the same file unless the first missed the exact symbol boundary you need
   - keep combined `read_sandbox_file` + file-level `query_symbols` calls at `8` or fewer
18. Once 4-6 concrete rooted candidates exist, stop and return them. Do not keep validating each candidate with deeper implementation reads.
19. Before finalizing, drop any candidate region whose workspace evidence stayed empty, hidden-only, or inaccessible after one bounded check; do not keep placeholder regions for absent tails.
20. Before finalizing, deduplicate candidate paths. If two remaining regions still share the exact same path, either merge them because they truly share one owned cluster or rewrite them to narrower distinct child slices.
20a. Before finalizing any same-file merge, audit whether that file carried multiple concrete goal-relevant fix surfaces. If yes, keep all of them visible in the merged region focus instead of mentioning only the most prominent one.
21. Before finalizing, audit whether any remaining root region is only an auxiliary test/config/docs tail for already-visible implementation lanes. If yes, fold it back into those lanes and keep the frontier narrower.
22. Before finalizing, audit whether any candidate merge is justified only by a broad shared pipeline label such as schema or serialization. If the primary owned files or validation targets differ, split those regions back apart while budget remains.
23. Before finalizing, audit whether a build-config or dependency-bump region is displacing a more concrete local behavior hotspot. If 4+ concrete source lanes already exist, keep the build-config file as supporting context instead of a root region unless the file itself is the likely local fix surface.

## Flexibility

- You may use any discovery heuristic that fits the exposed tools.
- You may merge, reorder, or broaden regions if that improves downstream exploration quality.
- You may return fewer than 6 regions when the goal is concentrated.
- For broad upgrade, migration, or release goals with several independent hotspots, bias toward 4-6 regions.
- Prefer separate low-risk infra regions over one mixed tail bucket when they fit within the 6-region budget.
- When the frontier already exposes the main execution lanes, prefer 4-5 strong regions over 6 regions padded with weak auxiliary tails.

## Output Schema

A downstream runtime posthook formatter converts your phase response into this shape.
Your response should cover the material needed for these fields, but you do not need to emit this JSON directly.
Keep the response compact and machine-friendly: no markdown tables, no fenced code blocks, no changelog walkthroughs, and no extended prose outside the region material.
The schema example below is documentation only. Do not copy its fences into the real answer, do not wrap the real answer in a JSON object, and do not prefix it with summary prose.

```json
{
  "region_count": 0,
  "regions": [
    {
      "path": "src/module",
      "focus": "Explain what matters here for the planning goal",
      "priority": "high",
      "depth": 2,
      "label": "core module"
    }
  ],
  "partition_strategy": "llm|heuristic|llm+heuristic_repair|context_repair",
  "partition_repaired": false,
  "context_seeded_region_count": 0,
  "skip_all_exploration": false
}
```

## Failure / fallback behavior

- If the repo signal is weak, prefer one broad region over many speculative regions.
- If a tail surface is absent from the visible workspace, omit it rather than creating a speculative region around missing files.
- If no goal-relevant region can be justified, say that clearly so the downstream formatter can set `skip_all_exploration: true`.
