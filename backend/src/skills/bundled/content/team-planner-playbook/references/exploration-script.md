# Exploration Script

Use this reference when the planner needs to understand a subsystem before assigning execution work.

## Goal

Produce a structural ownership map first, then assign developer and validator work against disjoint slices. Do not flatten broad exploration into one parent planner turn.

## Script

1. Seed the search space with live CI.
   Use the request, shared context, workspace structure, symbol lookup, and references to identify the candidate paths or directories.
   The planner does not read files directly. If the next question requires file contents to answer an ownership or decomposition question, scout that slice instead.
   If the failing tests already name a test file, treat that path as known evidence. Do not scout a giant test file just to recluster failures explicit in the request; prefer the likely source owner or a much smaller assertion-shaped slice.

2. Decide whether this is already execution-sized.
   If there is one obvious owned file cluster and one direct validation target, dispatch workers.
   If there are multiple plausible owners, a directory-sized slice, or a large file with many relevant regions, switch to exploration.
   Once live CI identifies one candidate implementation file or subsystem, the next step should be scout, child-planning, or dispatch.
   Prefer scout immediately whenever it can answer the ownership question.
   If the owner is already a single large file, a single-file scout is allowed when you still need that file's live structure or key symbols before dispatch. Move to child planning only when that scout still leaves several named regions unresolved.
   If several disjoint owner hypotheses remain, prefer a small wave of parallel scouts instead of proving them one at a time from the parent planner.

3. Launch a bounded scout.
   Call `run_subagent(agent_name="scout", input={"target_paths": [...]})` with concrete paths only.
   Give the scout the smallest slice that can still answer the ownership question.
   When ownership has already split, prefer several disjoint scouts in parallel over serial parent-side probing.
   Do not open a scout just because fanout is available. Launch only when the lane covers a still-unresolved owner boundary that existing scout briefs, atlas results, or shared context do not already answer.
   After launch, you MUST take at least one non-wait action before any `wait_for_background_task`: launch another disjoint scout, call `check_background_progress`, classify the remaining branch, promote a completed brief, or emit the final plan JSON. Do not call `wait_for_background_task` first unless that scout result is already the only blocker left.
   Treat parallel scouting as waves, not as a rigid one-shot batch. Start with the smallest useful wave, keep reasoning while those scouts run, and launch another disjoint scout or child planner only when the current evidence still leaves a real ownership gap.
   Fresh scout fanout is hard-capped at `8` launches per planner turn, so conserve lanes for genuinely distinct ownership questions.

4. Read the scout brief and classify the result.
   `scope_coverage >= 0.9` with a clear ownership map:
   Plan workers immediately. If the scout return includes `artifact_ref`, you
   may reuse or promote it directly because it is a real stored team artifact.
   `run_id` is only the scout audit id. If no artifact ref is available, distill
   the evidence into `share_briefing(source="inline", inline="...")` or keep it
   local to the current plan.

   `0.0 < scope_coverage < 0.7` with `suggested_subdivisions`:
   Fan out child scouts on those disjoint subdivisions, or hand the slice to a child planner if you cannot close it in this turn.

   `scope_coverage == 0.0` and no subdivisions:
   Treat the area as genuinely empty and revise the target paths.

5. Recurse only on narrower owned slices.
   Parent planner owns only the broad map.
   Each child scout owns one explicit subdivision.
   Each child planner owns one named sub-slice or one large-file region.
   Never reopen sibling branches from a child.

6. Convert large-file exploration into child planning.
   If one file contains too many relevant regions, symbols, or branches for the current level, emit an expandable `team_planner` item that names:
   - the owned file
   - the owned region, symbol subset, or question cluster
   - explicit out-of-scope neighbors

7. Stop conditions.
   Stop exploring when you can name:
   - the owned production slice
   - the likely fix or investigation question
   - the direct validation command or test target
   Sufficiency, not wave count, is the stop condition. Stop after the first wave if ownership is clear; launch another wave only if the existing evidence is still incomplete.
   If the only remaining question is the exact runtime mismatch inside an already mapped owner cluster, stop exploring and hand that cluster to a developer or validator with the exact failing test or command.
   On benchmark-style root turns, treat two scout waves or roughly 25 tool calls as the default ceiling. A third wave needs a genuinely new disjoint owner cluster, not a deeper dive into the same mapped cluster.

## Heuristics

- Prefer one bounded scout over more planner-side symbol/reference probing when the real goal is to understand ownership, interaction, or decomposition.
- Prefer one scout over many serial planner CI queries when structure is still unclear.
- Once a large candidate file is known, treat repeated parent probing as a smell. The next step should usually be scout or child planning.
- Once one large file is already the clear owner candidate, allow one scout on that single file when you still need a live structural map. Prefer child planning only after that scout, or when the next step is decomposing named regions rather than reading the file.
- If there are multiple disjoint candidate areas, prefer parallel scouts over parent-side file windows across those areas.
- Parallel scouts are background work, not foreground joins. If another ownership question remains, resolve that or do a non-blocking progress check before any blocking wait.
- While scouts are running, the planner may keep working other uncovered branches, reuse atlas/shared briefings, reason about task boundaries, and decide whether another disjoint scout wave is warranted.
- Launch another scout wave only when the current briefs leave a real disjoint ownership gap or expose disjoint `suggested_subdivisions`. Do not treat first-wave completion as an automatic stop, and do not treat it as automatic permission for more scouts either.
- If a `WAIT_REQUIRES_PROGRESS_CHECK` error fires, inspect once and either finish the plan or wait on the single remaining blocker. Do not use that error as permission for a fresh deep-dive scout wave over the same benchmark surface.
- If a whole-set wait times out, use completed scout returns, cancel stale low-value scouts when appropriate, or wait only on the remaining blocker. Do not immediately reissue another `wait_for_background_task(task_id="all")` across the same batch.
- If a budget warning appears, or you are down to only a few tool calls, stop exploring and emit the final JSON plan immediately.
- If a tool call is rejected because the planner budget is exhausted, treat that rejection itself as the finalization trigger and emit the JSON plan immediately.
- Do not queue a ready expandable child planner in parallel just for "if the developer finds more issues"; that contingency belongs in downstream deps or later replanning.
- Prefer atlas reuse only when the cached brief already answers the decomposition question.
- Prefer child planners over extra parent reads when a large file needs region-level ownership.
- Prefer disjoint fanout over overlapping scouts.
- Once a scout brief names the likely owner file cluster, do not resume low-signal planner-side CI queries driven only by changelog prose, dependency bumps, or version hypotheses.
- Once a source-owner scout exists, do not open new manifest or giant-test scouts unless packaging itself is still the unresolved owner question.
- Treat text matches in `pyproject.toml`, requirements files, lockfiles, and other version metadata as low-signal unless the task is explicitly about packaging. They do not replace source ownership.
- Do not use `ci_recent_changes` or `ci_edit_hotspots` to reconstruct release-note fixes. Those tools are for collision awareness after execution lanes already exist.

## Anti-patterns

- Firing repeated symbol/reference queries over the same large file from the parent planner just to decide who should own it
- Continuing root-planner probing after the candidate area is already known instead of handing off to scout or a child planner
- Launching a scout and then making `wait_for_background_task` the very next action while other ownership branches or planning work remain
- Treating the first scout wave as a mandatory stopping point even though the returned briefs still leave disjoint owner gaps unresolved
- Treating the first scout wave as automatic permission for more scouts even though the plan is already ownership-complete
- Treating "I need to understand the actual failures better" as a reason for another planner-side scout after the owner cluster is already known
- Reissuing `wait_for_background_task(task_id="all")` after a timeout instead of using completed briefs or canceling the stale scout
- Treating the planner like a file reader instead of using scout for file contents
- Responding to a budget or tool-limit warning with prose instead of the final JSON plan
- Emitting a speculative expandable child planner whose only purpose is "maybe there are more issues later"
- Using `developer` as a discovery worker
- Re-scouting a path already covered by shared context or a sibling scout
- Emitting a one-child recursive planner chain that simply restates the same broad slice

## Example: multi-file exploration

1. Use the request and CI to locate two candidate modules.
3. Scout the containing subsystem paths.
4. If the scout finds three disjoint ownership branches, fan out three child scouts or three child planner items.
5. Assign developers only after each branch has a clear owned slice.

## Example: one large file

1. Use the request, shared context, or CI to locate the target file.
2. If the file has several relevant regions, do not keep probing it from the root planner.
3. Emit an expandable child planner for one named region such as `"schema generation discriminator handling"` and exclude adjacent regions.
4. Let the child planner run scouts or CI only within that owned region.
