# Atlas Scout Promotion Design

## Goal
Reduce duplicate exploration during high-parallelism team runs by making direct scout output immediately reusable inside the same run, while keeping Atlas positioned as cross-run memory instead of a competing exploration scheduler.

## Current Runtime Position
- Fresh SWE-EVO runs already keep Atlas maintenance disabled.
- Planner fanout still depends on `scout` for ownership discovery.
- Before this change, fresh scout completions returned only a subagent run id, not a stable team artifact ref.
- Shared same-run reuse existed, but planners had to synthesize it manually and were blocked by a brittle "not a real artifact ref" assumption.

## Design Risks Observed

### 1. Artifact identity was overloaded
- `artifact_ref` sometimes meant "real team artifact" and sometimes meant "subagent audit run id".
- That prevented direct reuse of same-run scout output and made downstream prompt reuse ambiguous.

### 2. Same-run scout reuse depended on manual promotion
- High-quality scout results were not automatically promoted into shared run context.
- Under fanout, sibling workers were more likely to re-explore a scope that had already been mapped.

### 3. Shared-briefing capacity degraded poorly under wide fanout
- `share_briefing` rejected new entries when full.
- Rejection is acceptable for manual notes, but it is the wrong default for auto-promoted ownership maps in a parallel run.

### 4. Out-of-order scout completion could regress a scope view
- A slower scout over the same scope could replace newer knowledge unless the runtime guarded the stable per-scope artifact key.

### 5. Worker awareness still has two maturity levels
- Implemented now: same-run structural reuse via stable scout refs and shared briefings.
- Implemented now: CI-backed live scope-change awareness that uses briefing versions, ledger churn, hotspots, active reservations, and symbol state for prompt-time and write-time checks.

## Design Principles
- Same-run reuse comes from stable scout artifacts plus `shared_briefings`.
- Cross-run reuse comes from Atlas.
- `run_id` is audit identity.
- `artifact_ref` is prompt-facing artifact identity.
- Auto-promotion must use the same reusable-brief trust gate as Atlas.
- Fresh-run coordination must stay cheap: no new LLM turns unless a real scout is needed.
- Manual inline shared briefings are sticky; auto-promoted scout context is replaceable.

## Runtime Contract

### Stable scout artifact identity
- Every scout brief with a resolvable `canonical_scope` is stored under `scout:<canonical_scope>`.
- The returned envelope now contains:
  - `run_id`: subagent audit id
  - `artifact_ref`: real team artifact ref when one exists
- For scout completions, `artifact_ref` is no longer the subagent run id.

### Same-run reusable gate
- Auto-promotion uses the Atlas reusable-brief predicate:
  - explicit empty-area brief is reusable
  - otherwise `scope_coverage >= 0.9`
  - `suggested_subdivisions == []`
  - `gaps` empty
- This logic lives in one shared helper so Atlas reuse and same-run promotion cannot drift.

### Bounded same-run promotion
- If a reusable scout brief lands for a scope already present in shared context, it replaces the previous briefing in place.
- If capacity is full and the new scope is different, the runtime evicts only an existing auto-promoted scout briefing.
- Explicit `share_briefing(...)` promotions get the same headroom rule: they may evict an existing auto-promoted scout briefing before rejection.
- Manual inline briefings and non-scout artifact briefings are not evicted by auto-promotion.
- Once a scope is explicitly promoted, it is no longer treated as replaceable auto-promoted context.
- Eviction order for the first cut:
  1. lowest `scope_coverage`
  2. oldest `snapshot_time`
  3. lexical scope tie-break

### Out-of-order completion guard
- The stable `scout:<scope>` artifact key is overwritten only when the incoming scout is at least as new by `snapshot_time`.
- Equal or missing `snapshot_time` ties fall back to stable `run_id` ordering when both sides have provenance; otherwise the runtime keeps the current artifact.
- An older scout completion may still return the stable ref, but it must not replace newer stored content.

## High-Parallelism Operating Model

### What is implemented now
- Planners can reuse fresh scout `artifact_ref` values directly.
- High-quality scout results auto-populate same-run shared context.
- Sibling subagents inherit that shared context automatically.
- CI guidance now explicitly distinguishes:
  - Atlas for cross-run structure
  - shared briefings for same-run scout reuse
  - CI for live truth

### What remains intentionally separate
- heuristic shell-mutation reconciliation still exists as a compatibility fallback outside strict team lanes, but ultra-concurrency worker lanes now reject undeclared mutating shell commands
- the coordination snapshot is now surfaced through CI as the authoritative scope-status/admission view, but cross-run memory still remains Atlas rather than a separate global state store
- background Atlas persistence of direct scout output and scheduler policy tuning still evolve independently of same-run write coordination

## Implementation Phases

### Phase 1: Stable Scout Artifact Identity
Objective:
Make scout completions reusable without conflating audit ids and prompt-facing artifact ids.

Changes:
- split `run_id` from `artifact_ref` in `run_subagent` envelopes
- store scout briefs under stable `scout:<canonical_scope>` keys
- carry a per-scout `work_item_started_at`/`snapshot_time` into scout submissions

Acceptance criteria:
- a scout completion in team mode returns `run_id` and a real `artifact_ref`
- the stable artifact ref is deterministic for a canonical scope
- an older scout completion cannot overwrite a newer stable scout artifact

Status:
- implemented

### Phase 2: Same-Run Auto-Promotion
Objective:
Turn high-quality scout output into same-run reusable ownership memory automatically.

Changes:
- extract a shared reusable-brief predicate from Atlas freshness logic
- auto-promote reusable scout artifacts into `project_context.shared_briefings`
- bound promotion with deterministic eviction of existing auto-promoted scout entries only

Acceptance criteria:
- a reusable scout result becomes available through `## Shared context` without manual `share_briefing`
- same-scope scout updates replace older shared context in place
- capacity pressure evicts auto-promoted scout context instead of rejecting the new reusable scope outright

Status:
- implemented

### Phase 3: Planner And Toolkit Contract Cleanup
Objective:
Remove legacy instructions that told planners scout results were not real artifact refs.

Changes:
- update planner playbook and exploration reference to treat scout `artifact_ref` as reusable
- update `share_briefing` guidance so `run_id` is treated as audit-only and scout `artifact_ref` as shareable
- update CI guidance to call out stable same-run scout refs explicitly

Acceptance criteria:
- planner instructions no longer prohibit reuse of fresh scout `artifact_ref`
- prompt/toolkit guidance consistently separates `run_id` from `artifact_ref`
- no stale instruction path still tells workers to ignore a real scout artifact ref

Status:
- implemented

### Phase 4: Deferred Atlas Runtime Policy
Objective:
Re-enable Atlas only after it can consume direct scout output without launching duplicate scout work.

Changes:
- add an explicit scheduler policy surface
- keep fresh SWE-EVO on deferred-persistence-only mode
- factor Atlas write semantics out of `submit_atlas`
- add runtime persistence of completed scout briefs without a second scout pass

Acceptance criteria:
- fresh runs do not launch Atlas-owned scout work
- resumed or retried runs may enable Atlas maintenance without duplicating direct exploration
- Atlas persistence reuses scout-complete output and does not fork write semantics

Status:
- implemented

### Phase 5: Live Scope Awareness
Objective:
Give every developer and validator lane just-in-time awareness of current repo state, not only structural scout memory.

Changes:
- add coherent snapshot tokens across shared briefings, ledger, arbiter, and symbol index
- expose active per-file reservations
- add startup scope-change checks plus pre-write and commit/apply rechecks

Acceptance criteria:
- workers can distinguish fresh, locally touched, and structurally stale scope context
- workers receive collision warnings before editing
- same-scope refresh storms collapse into one coordinated refresh path
- write-time coordination is based on authoritative rechecks, not only startup prompt text

Status:
- implemented

## Worker Awareness Model

### Implemented now
- shared scout ownership memory is automatic and bounded
- scout reuse is artifact-backed instead of text-only
- machine-checkable freshness grades
- reservation-backed active editor visibility
- coherent CI-backed scope-change awareness for developer and validator startup
- startup and pre-write checks consult shared briefings, ledger, arbiter, and symbol index without prompt-injected metadata bundles
- production and benchmark workers both receive live scope-change warnings when CI is available
- overlapping edits invalidate scout-backed shared context and force fresh CI truth on subsequent turns
- write-time coordination uses authoritative pre-write and commit-time rechecks, not only startup prompt text

## Cleanup And Legacy Removal
- Removed the legacy assumption that fresh scout results are never real artifact refs.
- Removed the need to recover same-run scout reuse through inline restatement when a real stored scout artifact exists.
- Kept manual `share_briefing` behavior strict and explicit instead of turning it into a second trust gate.

## Success Criteria
- same-run scout results are reusable through stable real artifact refs
- reusable scout results automatically populate shared same-run context
- wide scout fanout does not regress to manual-only promotion
- older scout completions cannot clobber a newer stable scope view
- planner, CI, and sharing guidance agree on the new ref contract
- fresh SWE-EVO continues to avoid Atlas-owned duplicate exploration on the critical path
- the design and runtime agree on the implemented live scope awareness stack
