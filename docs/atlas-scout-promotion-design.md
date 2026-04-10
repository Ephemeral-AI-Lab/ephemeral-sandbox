# Atlas Scout Promotion Design

## Goal
Reduce duplicate exploration and token burn on fresh SWE-EVO runs by reusing completed foreground scout output inside the current run, while still persisting useful context to Atlas for later retries and resumes.

## Problem
Current fresh SWE-EVO runs can do both of these at once:

- Foreground planners launch `scout` subagents to map ownership.
- Atlas maintenance launches `atlas_builder` / `atlas_refresher`, which in turn launch more `scout` subagents for overlapping surfaces.

That means Atlas is acting like a second exploration lane during the same run instead of a cache for future runs.

Scout itself is already background-only today:

- planners invoke it through `run_subagent`
- `run_subagent` always backgrounds the task
- submitted plans cannot target subagents directly

So the immediate waste is not scout scheduling. The waste is Atlas maintenance spawning its own duplicate scout work alongside the foreground planner path.

## Design Summary

### Core split
- Same-run reuse comes from `shared_briefings`.
- Cross-run reuse comes from Atlas.

### High-parallelism requirements
For this design to remain resilient under high parallelism, it must provide:

- pre-edit scope packet: every developer and validator lane gets compact live scope status automatically before work starts
- same-run freshness gate: if ledger activity appears inside a scope after a scout snapshot, that briefing is marked stale immediately
- active collision signal: workers see in-flight edits or hotspot contention before they start editing
- stable ownership memory: the latest reusable scout per canonical scope is promoted and reused automatically
- cheap runtime path: hot-path coordination remains read-only and non-LLM unless a real scout refresh is required

### Foreground path
- Planner launches `scout` exactly as today.
- Completed scout output becomes a real team artifact under a stable per-scope key.
- If the scout brief passes the reusable-quality gate, runtime promotes it into `shared_briefings` for the rest of the run.
- Developer and validator lanes receive a pre-edit scope packet derived from the latest shared briefing plus live scope status before they begin code work.

### Background path
- Atlas persistence happens only after scout completion.
- Persistence reuses the completed scout brief and computes `content_hashes` and `symbol_ids` without a second LLM turn.
- Fresh SWE-EVO runs do not launch Atlas-owned scout refreshes on lookup misses.
- Hot-path coordination stays read-only and non-LLM; only genuine stale/missing scope context should trigger a fresh scout.

## Scope
This design is intentionally scoped to fresh SWE-EVO benchmark runs.

It does not change the default runtime behavior for:

- greenfield runs
- non-benchmark team runs
- resumed or retried SWE-EVO runs

Resumed and retried SWE-EVO runs should continue to prefer `atlas_lookup` early, because Atlas is most useful when a prior run has already populated it.

## Current Temporary Policy
Until deferred scout persistence lands, fresh SWE-EVO runs should keep Atlas maintenance disabled entirely.

This avoids:

- startup bootstrap work
- lookup-miss refresh work
- dirty-path idle refresh work
- duplicate Atlas-owned scout passes during the active benchmark run

This is a benchmark policy only. It does not imply Atlas should be removed from the general team runtime.

## Constraints

### 1. Auto-promotion must use the Atlas trust gate
Runtime must not promote every scout brief with a `canonical_scope`.

The auto-promotion rule should be exactly the same reusable-quality contract Atlas already uses:

- explicit empty-area brief is allowed
- otherwise `scope_coverage` must be present and above the reuse threshold
- `suggested_subdivisions` must be empty
- `gaps` must be empty

Implementation note:
- Do not duplicate this logic in `share_briefing` or benchmark-only code.
- Extract the existing Atlas reusable-brief predicate into a shared helper and reuse it for both Atlas reuse and same-run scout auto-promotion.
- When a promoted briefing becomes stale due to same-run ledger edits in scope, do not evict it immediately; mark it stale in live scope status so workers know to refresh before relying on it.

### 2. Fresh-run scheduler policy must be explicit
Fresh SWE-EVO runs currently wire the Atlas scheduler and allow cold-start bootstrap and lookup-miss refresh behavior.

This design requires a benchmark-specific scheduler policy, not a one-off conditional on lookup misses.

Required policy for fresh SWE-EVO runs:

- disable cold-start Atlas bootstrap
- disable miss-driven Atlas refresh when `atlas_lookup` returns `action="scout"`
- defer Atlas persistence until after a foreground scout completes
- defer dirty refresh work until foreground planning/execution is no longer on the critical path
- keep pre-edit coordination on the cheap read-only path so worker fanout does not create extra LLM load

Non-fresh runs can keep the existing Atlas scheduler behavior.

### 3. Promoted scout artifacts must use stable per-scope keys
Do not save scout artifacts under append-only refs like `scout:<scope>:<id>`.

Reason:
- the artifact store only reclaims bytes when the same key is overwritten
- append-only refs would grow artifact usage quickly under scout fanout

Use a stable key such as:

- `scout:<canonical_scope>`

This keeps only the latest reusable scout artifact per scope in prompt-facing storage.

The audit/history layer already exists in subagent run tracking and should remain the place for append-only debugging history.

### 4. Preserve run identity separately from artifact identity
Today `run_subagent` returns `artifact_ref=sub_run_id` for summary/brief envelopes.

If scout results become real team artifacts, `artifact_ref` should refer to the stored team artifact, not the subagent run id.

The envelope should therefore split these fields:

- `artifact_ref`: real team artifact ref when one exists
- `run_id`: subagent run id for audit, progress, and persistence lookups

Do not continue overloading one field with two meanings.

### 5. Atlas persistence must reuse existing write semantics
The deterministic Atlas write path already lives in `submit_atlas`:

- subsystem derivation
- `snapshot_time` handling
- `content_hashes`
- `symbol_ids`
- version-guarded upsert

Do not fork those semantics into a second Atlas writer.

Instead:

- factor the core "brief -> AtlasChunk(s) -> upsert" logic into a reusable helper
- keep `submit_atlas` as one caller of that helper
- add a runtime persistence path as another caller of that helper

This is a real refactor, not a small enum addition to the scheduler.

## Proposed Runtime Flow

### Fresh SWE-EVO run
1. Root planner starts without Atlas bootstrap pressure.
2. Planner launches `scout`.
3. `run_subagent` receives a completed scout brief.
4. Runtime derives `canonical_scope` and evaluates the shared reusable-quality gate.
5. Runtime stores the scout artifact under a stable per-scope artifact key.
6. If reusable, runtime promotes it into `shared_briefings`.
7. Runtime enqueues a lightweight Atlas persistence task that reuses the completed scout brief and writes it to Atlas without another scout.
8. Planner and sibling subagents reuse `shared_briefings` during the current run.

### Resumed or retried SWE-EVO run
1. Planner uses `atlas_lookup` early once it has stable subsystem keys.
2. `use` hits are attached as explicit briefings via real artifact refs.
3. `refresh` or `scout` results fall back to fresh foreground scouting.
4. Freshly completed scouts are again promoted to same-run shared context and persisted back to Atlas asynchronously.

## Required Code Changes

### A. Introduce explicit scheduler policy
Add a scheduler policy for fresh benchmark runs so the Atlas scheduler can be configured without changing global runtime defaults.

Suggested shape:

- default policy: current behavior
- fresh SWE-EVO policy: no bootstrap, no miss-driven Atlas scout refresh, deferred persistence only

### B. Save completed scout briefs as real team artifacts
Update `run_subagent` so successful scout briefs in a team run can be stored in the team artifact store under a stable per-scope key.

Requirements:

- stable key per canonical scope
- returned envelope includes both `artifact_ref` and `run_id`
- no append-only scout artifact keys in the team artifact store

### C. Add runtime promotion helper
Create a runtime helper that:

- checks the shared reusable-quality gate
- promotes a stored scout artifact into `project_context.shared_briefings`
- preserves existing caps and replacement semantics

This should not weaken greenfield invariants and should only be activated under the fresh SWE-EVO policy.

### D. Factor Atlas write helper out of `submit_atlas`
Extract the chunk-building and upsert logic into a shared helper so both posthook-driven Atlas writes and runtime persistence use the same semantics.

### E. Add deferred Atlas persistence path
After a foreground scout completes, enqueue a persistence task that:

- reuses the completed scout brief
- computes hashes and symbol ids
- upserts the corresponding Atlas chunk

This task should be non-LLM and should not spawn another scout.

## Dynamic Codebase Context Strategy
Agents need two different kinds of awareness in a changing repo:

- stable understanding of subsystem ownership and structure
- live awareness of what other workers have changed since that understanding was gathered

Atlas is only appropriate for the first category. The second category should come from live code-intelligence state.

### Recommended split
- Atlas: cross-run reusable briefs for stable subsystem context
- shared briefings: same-run reusable scout context
- ledger: recent file edit history with agent attribution
- arbiter: current conflict and hotspot awareness
- tree cache and symbol index: current file contents and symbol routing

### Proposed agent workflow in a changing codebase
1. Planner or worker identifies a target subsystem.
2. On resumed runs, try `atlas_lookup` first for stable structure context.
3. Before editing, inspect live change awareness for that scope:
   - recent changes under the target paths
   - edit hotspots
   - current owner or latest editing agent when available
4. If a same-run reusable scout exists, consume it from `shared_briefings`.
5. When files are edited, update ledger, arbiter generation, tree cache, and symbol index immediately.
6. If a previously gathered brief is now stale relative to ledger edits in scope, treat it as advisory only and re-scout or refresh selectively.

### Missing runtime capability
The main missing piece is a first-class "live scope status" helper that merges:

- latest scout/shared briefing for a scope
- recent ledger entries in that scope
- hotspot or active-edit signals from the arbiter
- symbol-index pointers for current definitions

That helper should be the default pre-edit context for developers and validators. Atlas alone cannot provide this because it is intentionally stale-tolerant and cross-run oriented.

### Proposed tool: `ci_scope_status`
The first implementation should be a read-only code-intelligence tool rather than a planner/runtime-only internal hook.

Suggested shape:

```json
{
  "scope": "pydantic/root_model.py",
  "briefing": {
    "source": "shared_briefings",
    "artifact_ref": "scout:pydantic/root_model.py",
    "summary": "...",
    "snapshot_time": "2026-04-10T10:00:00Z",
    "reusable": true,
    "stale_due_to_recent_edits": false
  },
  "recent_changes": [
    {
      "file": "/repo/pydantic/root_model.py",
      "agent_id": "developer-2",
      "edit_type": "edit",
      "timestamp": 1712700000.0,
      "description": "adjust RootModel generic handling"
    }
  ],
  "hotspots": [
    {
      "file": "/repo/pydantic/root_model.py",
      "edit_count": 3
    }
  ],
  "active_edits": [
    {
      "file": "/repo/pydantic/root_model.py",
      "locked": true,
      "agent_id": "developer-2"
    }
  ],
  "symbols": [
    {
      "name": "RootModel",
      "kind": "class",
      "file": "/repo/pydantic/root_model.py",
      "line": 42
    }
  ],
  "recommendation": {
    "action": "reuse_briefing",
    "reason": "shared briefing is fresh and no in-scope edits since snapshot"
  }
}
```

Interpretation:

- `briefing`: best structural context for the scope, preferring same-run shared context
- `recent_changes`: live ledger-backed edit history in scope
- `hotspots`: conflict-prone files from arbiter churn data
- `active_edits`: in-flight ownership/collision signal
- `symbols`: current symbol-index view of the code as it exists now
- `recommendation`: simple runtime guidance so agents do less policy reasoning ad hoc

### Pre-edit scope packet
`ci_scope_status` should not remain an optional manual tool call for execution lanes.

The runtime should build a compact pre-edit scope packet from it and inject that packet automatically into developer and validator startup context for their owned scope.

The packet should include only the minimum high-signal fields needed on the hot path:

- latest shared briefing summary and ref
- stale/fresh verdict for that briefing under same-run ledger activity
- recent in-scope edits with agent attribution
- active edit or contention warning
- top symbol pointers for the scope
- a single recommendation action

This keeps worker prompts small while still providing just-in-time awareness.

### First implementation boundary
The first cut should stay conservative:

- use `shared_briefings` as the only briefing source for fresh SWE-EVO runs
- use ledger, arbiter, tree cache, and symbol index for live signals
- do not pull Atlas into `ci_scope_status` for fresh runs
- add Atlas fallback later only for resumed or retried runs

This keeps same-run coordination separate from cross-run memory.

### Same-run freshness gate
The runtime must treat same-run ledger edits as an immediate invalidation signal for previously gathered scout context.

Required behavior:

- compare briefing snapshot time against ledger entries in scope
- if ledger has newer in-scope edits, mark the briefing stale in `ci_scope_status`
- downgrade recommendation from `reuse_briefing` to `refresh_scout` unless the edit is provably out of the scoped surface

This is the critical safeguard that keeps same-run shared context trustworthy under parallel execution.

### Active collision signal
Workers need collision awareness before they edit, not after a failed write attempt.

Required behavior:

- expose per-file active edit ownership from the arbiter through a read-only helper
- surface hotspot files for the target scope
- if a target file is actively edited or strongly contended, return `avoid_edit_conflict`

The worker can then defer, narrow scope, or choose another owned surface instead of wasting cycles on a collision.

### Stable ownership memory
Reusable scout output should become the latest ownership memory for that canonical scope inside the run.

Required behavior:

- store promoted scout artifacts under stable per-scope keys
- replace older reusable scout context for the same scope
- prefer the latest reusable promoted scout when building the pre-edit scope packet

This keeps ownership memory bounded and predictable during wide fanout.

### Cheap runtime path
High parallelism only works if coordination stays cheap.

Required behavior:

- `ci_scope_status` is read-only
- scope-packet assembly does not spawn agents
- no LLM call occurs on the hot path unless the recommendation is `refresh_scout`
- Atlas persistence remains deferred and non-LLM

This prevents coordination cost from scaling linearly with worker fanout.

### Required code changes for `ci_scope_status`
1. Add a new read-only tool in `tools/ci_toolkit/query_tools.py`.
2. Export it from the CI toolkit so execution agents can call it directly.
3. Add a small shared-briefing resolver that returns the best briefing for a canonical scope without rendering prompt text.
4. Add a read-only arbiter helper for active in-flight edits, because current hotspot APIs do not expose per-file active edit ownership cleanly.
5. Add tests for recommendation behavior and scope filtering.
6. Add a runtime hook that injects a compact pre-edit scope packet into developer and validator startup context automatically.

### Recommendation policy for the first cut
- `reuse_briefing` when a shared briefing exists and no recent in-scope edits make it stale
- `avoid_edit_conflict` when the target file is actively edited or highly contended
- `refresh_scout` when there is no shared briefing or the live change stream likely invalidated it

This policy should remain intentionally simple until same-run scout promotion is in place.

### Recommended order of investment
1. Keep Atlas maintenance off for fresh SWE-EVO.
2. Promote reusable scout results into `shared_briefings`.
3. Add `ci_scope_status` so execution agents can query merged scope context directly.
4. Re-enable Atlas only after scout-complete persistence exists and no second scout pass is required.

## Durability and Resume

### Run-local behavior
Promoted scout artifacts and shared briefings should be available for the rest of the live run.

### Checkpoint behavior
Checkpoint snapshots already include:

- artifact store contents
- project context

So promoted scout artifacts and shared briefings will survive checkpoint/rollback within the same process lifecycle.

### Event-log resume
If process-loss resume must preserve promoted scout artifacts and shared briefings outside in-memory checkpoints, the runtime will need durable eventing for those promoted artifacts and shared-context mutations.

That is a separate extension and not required for the first cut.

## Latest-Per-Scope vs History
Prompt-facing artifact storage should use latest-per-scope semantics.

Reason:

- it matches byte-budget behavior
- it matches how shared context is consumed
- append-only history already exists in subagent run tracking

If historical scout comparison is needed later, it should live in the run/audit layer, not in prompt-facing artifacts.

## Non-Goals
- changing `share_briefing` into a trust/quality checker for all callers
- changing greenfield behavior
- globally disabling Atlas maintenance
- replacing Atlas with shared briefings
- making same-run shared context durable across process-loss resume in the first cut

## Phased Rollout

### Phase 1
- add fresh SWE-EVO scheduler policy
- split `artifact_ref` and `run_id` in the scout envelope
- save completed scout briefs under stable per-scope artifact keys

### Phase 2
- extract shared reusable-quality gate
- add runtime auto-promotion for reusable scout briefs under fresh SWE-EVO policy
- add same-run freshness gate for promoted briefings

### Phase 3
- factor Atlas write helper out of `submit_atlas`
- add deferred non-LLM Atlas persistence from completed scout artifacts
- add `ci_scope_status`
- add arbiter-backed active collision reporting
- inject compact pre-edit scope packets into developer and validator startup context

### Phase 4
- if needed, add durable eventing so promoted scout artifacts and shared-briefing mutations survive process-loss resume outside checkpoint snapshots

## Success Criteria
- fresh SWE-EVO runs stop launching Atlas-owned scout work on planner lookup misses
- same-run planners and subagents can reuse completed scout context through real artifact refs and shared briefings
- Atlas persistence no longer requires a second scout pass for the same scope
- artifact-byte growth remains bounded under scout fanout
- resumed and retried runs still benefit from Atlas reuse
- every developer and validator lane receives compact live scope awareness before editing
- same-run edits invalidate stale scout context quickly and deterministically
- workers can detect active collisions before wasting an edit attempt
- hot-path coordination stays read-only and non-LLM
