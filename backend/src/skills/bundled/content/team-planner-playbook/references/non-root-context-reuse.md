# Non-Root Context Reuse

Use this reference when the planner is running below the root level, especially when the prompt includes `## Scoped Expansion` or already contains inherited briefings.

Goal: spend child-planner budget only on uncovered ownership gaps. Reuse the context the runtime already injected before launching fresh exploration.

---

## What a Child Planner Already Has

Non-root planners do not start from zero. The runtime can already inject three kinds of context into the prompt:

1. `## Shared context`
   Run-scoped promoted briefings, including atlas reuse and earlier shared scout summaries.

2. `## From deps`
   Snapshotted artifacts from completed upstream work items. These are often the most concrete branch-local evidence you have.

3. `## From parent`
   Explicit briefings attached by the parent planner to narrow this child's scope.

Treat these as real planning inputs, not as decorative background text.

---

## Child Planner Opening Script

1. Read inherited context before calling tools.
   Start with `## Shared context`, `## From deps`, and `## From parent`.
   If those sections already name the owned file cluster, region map, or validation target, plan from them directly.

2. Lock the slice boundary first.
   Combine the parent's `expansion_hint`, explicit child payload, and inherited briefings into one owned boundary.
   Do not reopen siblings outside that boundary.

3. Reuse the strongest existing evidence.
   If an inherited atlas brief or prior scout artifact already covers this slice with enough detail to assign workers, dispatch workers.
   If the inherited brief already names the relevant regions or symbol clusters inside one file, skip another whole-file scout.

4. Scout only the uncovered gap.
   If inherited context is partial, launch a scout only on the missing sub-slice.
   Do not scout the whole parent-owned file or subsystem again just because the child turn is new.

5. Promote only novel context.
   If the child learns something siblings will need and it is not already present in shared context, distill it once.
   Otherwise keep the inherited evidence local and finish the plan.

6. Attach the handoff explicitly.
   If the emitted child planner, developer, or validator will rely on inherited ownership maps, artifact refs, touched-file scope, or nearby guardrails, attach that evidence as `briefings` or concrete payload fields.
   Do not assume the next worker will recover the same branch-local map from atlas lookups or global `ci_recent_changes`.

---

## Reuse Rules

### Prefer inherited context over fresh atlas reads

- If `## Shared context` already contains a reusable atlas brief for this slice, do not call `atlas_lookup` just to rediscover the same map.
- Use a fresh atlas lookup only when the inherited context does not cover the child-owned slice or the prompt itself indicates the atlas brief is stale.

### Prefer branch artifacts over repeated scouts

- If `## From deps` or `## From parent` already contains a scout-shaped ownership map, do not launch another scout on the same scope.
- If the inherited artifact covers the broad file but leaves one region unresolved, scout only that named region or emit execution-sized work for the already clear parts.

### Prefer narrowing over rediscovery

- Child planners exist to narrow a known slice, not to restart root exploration.
- A child turn should usually consume inherited context plus at most one new scout wave over still-uncovered regions.

---

## When to Explore Again

Fresh exploration is justified only when one of these is true:

- the inherited context does not actually cover the child-owned slice
- the inherited map names multiple unresolved regions inside the child boundary
- the child still cannot assign disjoint worker ownership from the inherited evidence
- a stale atlas brief is the only available evidence and no fresher branch-local artifact exists

Fresh exploration is not justified when:

- the child prompt already names the owner file or region and the remaining question is runtime behavior
- a prior scout already mapped the same file and only one named region is still in scope
- the child would just restate the parent map in its own words

---

## Practical Heuristics

- Shared context is for cross-branch reuse; dependency artifacts are for branch-local truth; explicit parent briefings are for scope control.
- If those three together give you ownership plus validation, dispatch immediately.
- If a downstream validator needs branch-local touched files or nearby regression targets, pass that branch-local scope directly instead of widening validation through repo-global recency checks.
- If only one meaningful sub-slice remains, emit execution work instead of another planner wrapper.
- If you scout, ask only for the missing structure that inherited context did not already answer.
- If the next move would be "re-read the same file because this is a child planner now", stop and reuse the existing brief instead.

---

## Anti-Patterns

- Calling `atlas_lookup` on a subsystem that is already present in `## Shared context`
- Re-scouting a file already mapped by a dependency artifact or explicit parent briefing
- Treating `## Scoped Expansion` as permission to reopen sibling branches
- Spending a child turn reproducing the parent planner's decomposition instead of narrowing one owned sub-slice
- Launching fresh exploration before reading the inherited briefing sections

## Direct child execution from parent expansion hints

- When the parent briefing already names the residual clusters, target files, and likely worker split, treat that as the decomposition boundary for this layer.
- If the parent already mapped one owned production file or one tight owner pair per residual cluster, emit direct developer/validator items from that mapping. Do not launch one scout per already-mapped cluster just to restate key symbols or file summaries.
- Do not spawn nested `team_planner` agents to restate the parent split. Either emit direct developer/validator items or use a single scout per unresolved owner file and then emit the plan.
- Never call a nested planner with `prompt=null` or with no concrete decomposition question. That is a protocol error, not exploration.
- If a parent expansion hint already says "one child for X, one child for Y, one child for Z", your job is to convert X/Y/Z into concrete worker items, not to open a new planning tree for X/Y/Z.
- If inherited `owned_failures` already contain exact pytest node ids, preserve them verbatim downstream. Do not rename, shorten, de-parameterize, or substitute nearby test names while expanding the child plan.
