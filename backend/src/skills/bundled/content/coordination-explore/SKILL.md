---
name: coordination-explore
description: Explore-phase contract for the planning workflow. Launches batched explorer workers with run_parallel_agents and aggregates their serialized explorer reports.
---

# Coordination Explore Phase

## Role

You are the `explore` phase of the 4-stage planning workflow.
You coordinate worker explorers. You do not directly inspect the repo in this phase.
This phase is only the first hierarchical exploration pass. It should establish frontiers, not exhaustively map every assigned region.
A separate runtime posthook formatter converts your phase response into the final persisted output.

## Inputs

Use these sources:

- the runtime context message for `goal`, `project_context`, `phase_outputs`, and `phase_settings`
- `query_phase_context("analyze", "regions")` for the authoritative exploration region list

`regions[*]` from `analyze` is the only exploration input.
This skill defines a hard phase cap of `6` explorer workers per wave.

## Hard Constraints

- Runtime execution may use only `query_phase_context`, `list_phases`, and `run_parallel_agents`.
- `get_skill_instructions` and `get_skill_reference` are allowed only as skill-loading meta tools.
- Do not call `get_skill_script` in this phase.
- Never use direct repo-reading tools in this phase.
- Never call `run_parallel_agents` with more than the skill-defined phase cap of `6` in one call.
- If there are more regions than the phase cap, launch multiple waves sequentially.
- Do not pass `goal` or `project_context` as explicit `run_parallel_agents` arguments. The runtime already injects the parent goal and project context into worker prompt rendering.
- Treat broad or root-like regions as frontier-scout assignments, not full audits.
- The root explore phase should stay shallow. Do not spend worker budget trying to thoroughly cover an entire top-level package in one run.
- The runtime worker ceiling is Agno `tool_call_limit`, not `timeout_per_worker_s`. Use an explicit `tool_call_limit` when launching explorer workers.
- Preserve the `analyze` priority contract: only `high` and `normal` are valid priorities.
- Ensure your response makes the wave execution order, per-region outcome, and any normalization/failure decisions unambiguous for the downstream formatter.
- Normalize every worker outcome into non-empty serialized JSON content. Never pass through `null` content.
- If a worker report says the assigned region was hidden, unindexed, empty, or otherwise inaccessible, do not count that row as a fully completed exploration result. Record it as `partial_success` when the JSON is still usable, or `failed` when it is not.
- Never upgrade an inaccessible-region scout result to `completed` merely because the worker returned valid JSON.
- If an analyzed region focus carries multiple concrete change surfaces for the same file, preserve each one in the normalized explorer payload or mark the missing surface as an explicit `coverage_gap`. Do not let one named surface disappear because another in the same file dominated the scout read.
- Do not try to format or submit the final posthook payload yourself.

## Tools Available

Skill-loading meta tools:

- `get_skill_instructions`
- `get_skill_reference`

Runtime planning tools:

- `query_phase_context`
- `list_phases`
- `run_parallel_agents`

Use `run_parallel_agents` as the canonical tool name. Do not use `run_parallel_workers` in this phase.

## Recommended Procedure

1. Call `query_phase_context("analyze", "regions")`.
2. Order regions so `high` priority regions run before `normal` ones, while preserving original order within each priority tier.
3. Split ordered regions into waves whose size is at most the skill-defined phase cap of `6`.
   - If the ordered region list has `1-6` items, launch exactly one wave containing all of them.
4. For each wave, call `run_parallel_agents` with:
   - `items`: the region paths for that wave
   - `agent_name`: `"codebase-explorer"`
   - `skills`: `[]` as a native empty list, not the string `"[]"`
   - `instructions_template`: the explorer prompt template
   - `max_workers`: `6`
   - `tool_call_limit`: `6` for the root/default scout pass
   - only when `project_context` contains `## Scoped Expansion`, you may raise `tool_call_limit` for that child wave up to `20`
   - omit `goal` and `project_context`; they are inherited automatically from the parent runtime context
5. Frame every worker as a scout pass:
   - broad or root-like regions should return a frontier map and explicit `coverage_gaps`
   - focused regions may inspect representative files, but should still avoid exhaustive coverage
   - do not ask the worker to thoroughly explore a whole package just because the assigned path is broad
   - keep worker budgets small: the worker ceiling is whatever `tool_call_limit` you launched the wave with. Root/default scout waves should keep this at `6`, while child scoped-expansion waves may go higher when needed.
   - for broad or root-like regions, do not ask the worker to call `query_symbols` on the whole directory or package path; if symbol evidence is needed, it should target one specific file only
   - if one analyzed region focus names multiple concrete change surfaces in the same file, instruct the worker to mention each surface in `changelog_touchpoints`, `risk_areas`, or `coverage_gaps` before stopping
   - tell workers to stop as soon as the frontier is clear rather than spending the full budget by default
6. Wait for one wave to finish before launching the next.
7. Normalize worker output before submission. For each worker result, recover exactly one JSON payload when possible:
   - if the worker returned raw JSON, keep it unchanged
   - if the worker wrapped the JSON in fences, tool-call wrappers, or surrounding prose, strip Markdown fences and strip only the wrapper while keeping the JSON payload
   - if the worker timed out but produced a usable recovered summary, record that row as `partial_success`, preserve the recovered JSON payload, and count it as usable-but-lower-confidence rather than a hard failure
   - if the recovered JSON says the region was hidden, unindexed, empty, or inaccessible, record that row as `partial_success` instead of `completed`
   - if the worker timed out or failed with no usable recovered payload, store a compact error JSON string instead of prose or `null`
   - if no exact JSON payload can be recovered, record a hard-failed report with compact error JSON
8. Aggregate the normalized worker results into `region_reports` and `explorer_session_ids`.
9. Count outcomes explicitly:
   - `success_count` includes both `completed` and `partial_success` region reports because both are usable downstream
   - `partial_success_count` counts only recovered timeout summaries
   - `failed_count` counts only hard failures with no usable recovered payload

Batch manually inside the phase. Ignore any listed local scripts for this step; they are not part of the runtime contract here.

## Depth-Aware Exploration (Scoped Expansion)

When running inside a child expansion (detected by `## Scoped Expansion` in `project_context`):

- The parent already explored this area at the depth noted in `parent_exploration_depth`. Your exploration should go **deeper**, not broader.
- Files listed in `parent_explored_files` were already read by the parent. Do not re-read them unless you need deeper symbol-level detail (e.g., tracing a specific call chain).
- Focus workers on the `parent_expansion_hint` region. Do not scout broadly outside this scope.
- Scoped child exploration may use a larger worker budget than the root scout pass, but keep it bounded and explicit: only raise `tool_call_limit` when needed, and never above `20`.
- Adjust explorer strategy by parent depth:
  - Parent depth 1 (file listing only) — workers should read files and extract key functions/classes.
  - Parent depth 2 (file reading) — workers should trace call chains, follow imports, and map dependencies between files.
  - Parent depth 3 (symbol parsing) — workers should analyze specific logic paths, control flow, and edge cases.
- Allow workers a higher `tool_call_limit` (up to `20`) for deeper investigation since regions are narrower.
- Your findings **augment** the parent's — do not repeat what the parent already reported. Focus on new detail.

When NOT inside a scoped expansion, use the default scout-style behavior described in the recommended procedure.

## Flexibility

- You may batch manually or use the local batching script.
- You may use fewer workers than the cap in a wave.
- Prefer narrower scout-style passes over deeper coverage when a region is broad (unless inside a scoped expansion where deeper is expected).
- You may stop early only if `analyze` returned no regions.
- Prefer small, synthesis-friendly explorer payloads over rich but oversized reports.

## Output Schema

```json
{
  "region_count": 0,
  "success_count": 0,
  "partial_success_count": 0,
  "failed_count": 0,
  "region_reports": [
    {
      "region": "src/module",
      "status": "completed|partial_success|failed|timeout",
      "content": "{\"region\": \"src/module\", \"structure\": \"...\"}",
      "duration_ms": 0,
      "session_id": "worker-session-id"
    }
  ],
  "explorer_session_ids": ["worker-session-id"]
}
```

A downstream runtime posthook formatter converts your phase response into this shape.
Your response should cover the material needed for these fields, but you do not need to emit this JSON directly.

## Failure / fallback behavior

- If `analyze` returned no regions, return zero counts and empty arrays.
- If one wave fails, continue with later waves when safe and record the failed regions.
- If a worker times out but yields a usable recovered summary, record `partial_success`, preserve its `session_id` when available, and keep the recovered JSON payload in `content`.
- If a worker times out or fails with no usable recovered summary, record that hard-failure status and serialize a compact error JSON payload into `content`.
- If a worker returns malformed or wrapped output, normalize it to raw JSON when exact recovery is possible; otherwise treat it as failed rather than paraphrasing it.

## Explorer Prompt Template

```text
Explore the codebase region: {{item}}
Goal: {{goal}}
Project context: {{project_context}}

Stay bounded:
- Exploration in this system is hierarchical.
- If `{{item}}` is broad or root-like, produce a frontier map only.
- Do not thoroughly explore the whole region in one run.
- Inspect only files under `{{item}}`
- Start with one structure survey and stop early if that already identifies the frontier.
- The runtime hard ceiling is whatever `tool_call_limit` you launched the worker with. Root/default scout waves should keep this at `6`.
- Prefer a few representative files over broad scans
- If `{{item}}` is broad or root-like, do not call `query_symbols` on the whole directory or package path. If symbol evidence is still needed, target at most one specific file.
- A broad or root-like region should usually finish in 2-4 tool calls total. A focused region should usually finish in 4-6.
- Defer child-heavy depth into explicit `coverage_gaps`
- Summarize structure, key files, APIs, dependencies/import edges, changelog touchpoints, risks, and coverage_gaps
- If the assigned region focus names multiple concrete change surfaces in one file, cover each one in `changelog_touchpoints`, `risk_areas`, or `coverage_gaps`. Do not report only the most prominent issue and omit the others.
- Keep the report compact: short frontier summary, usually 5-12 key symbols, and short arrays for patterns/cross_references/risk_areas/coverage_gaps
- Return exactly one raw JSON object
- Do not use Markdown fences
- Do not emit prose before or after the JSON object
- Do not emit tool-call markup or XML wrappers
- Do not reread the same file in multiple windows unless the first read clearly missed the needed boundary

Depth-aware exploration (if project_context contains `parent_explored_files`):
- The parent already explored this area. Do NOT re-read files the parent already covered unless you need deeper detail.
- Go DEEPER than the parent: if they listed files, you should read them. If they read files, you should trace call chains. If they traced calls, you should analyze logic paths.
- Focus on new findings the parent missed, not repeating what they reported.
- Include `files_read` in your JSON output listing every file you actually read, so downstream systems can track exploration depth.
```
