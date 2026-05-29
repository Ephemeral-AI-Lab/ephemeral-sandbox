# Agent Initial-Message Restructure — Consensus Plan

**Status:** IMPLEMENTED 2026-05-29 (was APPROVED) — Units 1, 2, and the A2/A3 cleanups landed; see **Implementation Status** at the end of this file. Originally APPROVED (Architect + Critic consensus on the content reduction & downstream-safety in 1 cycle). Evaluator target subsequently revised from E1 to **E4 (flat, current-attempt-only)** by user direction — E4 shares E1's content rationale and adds an evaluator-local structural refactor (see R6). The structural refactor was not separately re-run through the Critic.
**Scope:** `task_center` (context engine, agent launch), `agents` module (profiles, loader), role skills.
**Goal:** A clean, explicit `[system, context, guidance, skill?]` initial-message structure; reduce the evaluator context to the current attempt's substance only; re-evaluate the whole recipe landscape and fix the issues that surfaces.

---

## RALPLAN-DR Summary

### Principles
1. **One job per row.** `system` = durable role contract; `context` = current state; `guidance` = what to do now; `skill` = how to do it.
2. **Recipes carry only what the role must act on (locality).** The `generator` recipe (`<plan_spec>` + `<dependency>` + `<assigned_task>`, *flat*, no goal frame) is the reference. The evaluator should match it: flat blocks carrying only what it judges.
3. **One source of truth per fact.** The dynamic "What's in context" outline owns block enumeration; static prose (the role contract) must not duplicate the tag list.
4. **Behavior-preserving where the goal is clarity.** The row-structure change must not alter the transcript the model sees; it only makes the already-emergent order explicit.
5. **Surgical, separately-landable units.** The row refactor, the evaluator reduction, and the cleanups have different blast radii and land independently.

### Decision Drivers (top 3)
1. **Defer-path correctness (highest).** A binary evaluator that can see `<goal>`/`<iteration_goal>` — or `<deferred_goal_for_next_iteration>`, which literally names the out-of-scope remainder — may fail a *correctly-bounded deferred slice* for not covering the whole goal. Removing the goal/iteration frame **and** the deferred-goal block eliminates the failure mode at the source.
2. **Criteria-as-authority / locality.** The evaluator judges *this* attempt against *these* `<evaluation_criteria>`. Anything outside the current attempt — goal scope, prior iterations, prior failed attempts — is planner-scope or retry-fuel, not evaluator evidence, and dilutes a binary verdict.
3. **Minimal blast radius.** `goal_iteration_blocks` (`recipes/iterations.py`) and `failed_attempt_blocks` (`recipes/attempts.py`) are shared with the **planner**. The evaluator change must leave the planner's context byte-identical; the refactored current-attempt emitter is evaluator-only.

### Viable Options (evaluator reduction)
Two axes — **content** (what the evaluator sees) and **structure** (flat vs wrapped):

- **E4 — Flat, current-attempt-only (RECOMMENDED / LOCKED).** *Content:* drop `<goal>`, prior-iteration background, `<iteration_goal>`, failed-prior attempts, **and `<deferred_goal_for_next_iteration>`**. *Structure:* emit the current attempt's substance as **top-level blocks** — `<plan_spec>` (framing) + one `<task id status>` per generator task (**summary-only** body) + `<evaluation_criteria>` (authority) — no `<iteration>`/`<attempt>` wrapper, mirroring the generator recipe's flat style.
  - *Pro:* evaluator sees exactly what it judges — framing + evidence + authority; `<evaluation_criteria>` becomes the most prominent block instead of buried inside an attempt; eliminates the defer-path failure mode; zero planner impact (the refactored emitter is evaluator-only).
  - *Con:* an evaluator-local refactor of `current_attempt_block` / `_render_current_attempt_body` (one `pre_rendered_xml` block → separate blocks), re-homing per-field sanitization, and confirming outline/`tag_dictionary` coverage (R6).
- **E1 — Minimal deletion (FALLBACK for structure).** Drop only the `goal_iteration_blocks` + `failed_attempt_blocks` calls; the current attempt stays wrapped as `<iteration status="current"><attempt status="current">…</attempt></iteration>`. Same *content* as E4, keeps the wrappers. Zero emitter change. Use if the flat refactor is deferred to a follow-up.
- **E3 — keep failed priors (FALLBACK for content).** Content = current + failed-prior attempts (wrapped). *Rejected by default* — failed priors had different plans/criteria; for a binary evaluator they are misleading noise that invites cross-attempt comparison. Re-adopt only if the evaluator is shown to need regression awareness.
- **E2 — keep prior-iteration background.** Drop only `<goal>`+`<iteration_goal>`, keep `<iteration status="prior">`. Needs a shared-helper split (A1). *Rejected* — cross-iteration background is planner retry-fuel, not evaluator evidence.

**Recommendation:** E4. The evaluator's job is binary judgment of the current attempt against its criteria; nothing outside the current attempt is evidence. **(a) Structural removal beats skill-prose mitigation** — removing the blocks makes misweighing *unrepresentable*, stronger than the skill's existing "don't require completeness against `<goal>`" guardrail, which can decay under long-context pressure. **(b) Downstream-safe (Architect-verified)** — `GoalClosureReport` (`goal/state.py:81-97`) is ID-only, and the evaluator's free-text summary feeds the *next iteration's planner* (which keeps the full `<goal>` frame), so no consumer reads evaluator output against the goal. **`<task>` evidence is summary-only, not task_spec+summary** — task_specs are the generator/verifier contract; the evaluator judges at the criteria level and has its own read/shell tools for ground truth. **`<deferred_goal_for_next_iteration>` is excluded** — it names the out-of-scope remainder and re-creates the incompleteness-bias trap for a binary evaluator; it is a lifecycle artifact read from attempt *state* (to spawn the continuation iteration), not evaluation evidence. The planner still sees it in its failed-prior blocks (legitimate retry context), so the shared `_render_plan_spec_children` stays untouched.

---

## Current State (verified against code)

### Initial-message assembly
- **Row 1 (system):** `agent_def.system_prompt` = `_main_role_contract.md` body + `\n\n` + profile body, for `main/*.md` profiles (`agents/definition/loader.py:25-63`). Recombined at spawn (`engine/agent/factory.py:299-320`).
- **Row 2 (context):** recipe → `XmlPromptRenderer.render_context` → `<context>…</context>\n` (`agent_launch/composer.py:56-87`, `context_engine/renderer.py:68-74`).
- **Row 3 (guidance):** `build_task_guidance` = dynamic outline `render_what_in_context` (`context_engine/what_in_context.py:48-52`; labels from `context_engine/tag_dictionary.py:37-116`) + static `ROLE_DIRECTIVES[name]` (`context_engine/role_directives.py:17-24`), wrapped `<Task Guidance>` + appended `<terminal_tool_selection>`.
- **Row 4 (skill):** `build_skill_message` (`agent_launch/skill_message.py`), `None` when no skill.

### Ordering is emergent, not explicit
`attempt/launch.py:126-151` is a 3-way branch on `(task_guidance, skill)`. Because `EphemeralAgent.run` appends `runner_prompt` after `initial_messages`, the chosen prompt lands **last**. Net order today: planner/executor/evaluator → `[system, context, guidance, skill]`; verifier → `[system, context, guidance]`. The desired order already holds — but only as an implicit side effect.

### Recipe landscape (3 recipes, 6 roles)
| role | recipe | emitted blocks (current) | skill |
|---|---|---|---|
| planner | `planner` | `<goal>`, prior `<iteration status="prior">`*, `<iteration status="current">`(`<iteration_goal>`), failed `<attempt>`* | yes |
| executor | `generator` | `<plan_spec>`?, `<dependency>`*, `<assigned_task>` | yes |
| verifier | `generator` | (same as executor) | no |
| evaluator | `evaluator` | `<goal>`, prior `<iteration>`*, `<iteration_goal>`, failed `<attempt>`*, **`<attempt status="current">`** | yes |
| advisor | none (bypasses composer) | parent terminal payload via `ask_advisor` | no |
| explorer | none (bypasses composer) | `build_explorer_task_guidance` via `run_subagent` | no |

The evaluator's `<goal>`+prior-iterations+`<iteration_goal>` come from the shared `goal_iteration_blocks(...)` call (`recipes/evaluator.py:48-52`), identical to the planner's (`recipes/planner.py:48-52`). Its failed priors come from `failed_attempt_blocks` (shared with planner). Its current attempt comes from `current_attempt_block` (`recipes/attempts.py:121-160`) — **evaluator-only**, emitted as one `pre_rendered_xml` block whose body (`_render_current_attempt_body`, attempts.py:179-192) already = `<plan_spec>` (+ `<deferred_goal_for_next_iteration>`) + per-task `<task>` summaries + `<evaluation_criteria>`.

---

## Issues Found (re-evaluation)

**Locked-change driver:**
- **I0 — Evaluator over-scoped.** Sees `<goal>` + `<iteration_goal>` + prior-iteration background + failed-prior attempts — none of which are evidence for a binary judgment of the current attempt against its criteria. The goal/iteration framing is a *defer-path correctness bug* (Driver 1); failed priors (different plans/criteria) are misleading noise.

**Ancillary (numbered, with severity):**
- **A1 — `goal_iteration_blocks` is an atomic 3-in-1 helper** (`recipes/iterations.py:48-64`); not parameterizable. *Severity: Medium.* Fix (split into primitives) **only needed if E2** is adopted; with E4 the evaluator simply stops calling it, so defer.
- **A2 — `_main_role_contract.md:3` statically enumerates the tag vocabulary.** *Severity: Med (coupled to Unit 1).* The moment the evaluator change lands, the shared contract enumerates `<goal>`/`<iteration_goal>` while only the planner (1 of 6 roles) receives them. Soften to "your context arrives as XML-tagged blocks; the `<Task Guidance>` outline names which are present this run." **Ships with Unit 1.**
- **A3 — `ROLE_DIRECTIVES["advisor"]` is dead** (`role_directives.py:22`). *Severity: Low.* Remove the key.
- **A4 — `ContextBlockKind.FAILED_ATTEMPT` is overloaded** for both failed-prior and the current attempt (`attempts.py:103,139`). *Severity: Low.* With E4 the current attempt no longer reuses this kind (it becomes `plan_spec`/`task`/`evaluation_criteria` blocks), which **naturally retires the overload** for the evaluator path; `failed_attempt_blocks` keeps the kind for the planner. Consider renaming the kind to `ATTEMPT` later. Mostly resolved by E4.
- **A5 — Directive table (6) vs task-guidance dispatch set (4) are unsynced** with no single registry; `build_task_guidance` `KeyError`s if a dispatched role lacks a directive (`builders.py:43`). *Severity: Low-Med.* Single-source the binding or add a startup validation.
- **A6 — `verifier` shares the `generator` recipe with `executor` but declares no skill.** *Severity: Low.* Note only — out of scope unless a verifier skill is wanted.
- **A7 — Two parallel walkers** (`renderer._render_blocks`, `what_in_context._walk_top_level`) + `tag_dictionary` must stay in lockstep; an unknown tag is silently dropped from the outline (`what_in_context.py:84-86`). *Severity: Low → relevant to E4:* the new top-level evaluator tags must exist in `tag_dictionary` (see Unit 1).

---

## Proposed Target Structure

1. **Canonical, centralized row order.** The order `[system, context, guidance, skill?]` is made explicit by an inline declarative list in `launch.py`; system stays separate (from `agent_def`).
2. **Evaluator recipe = the current attempt, flat.** Top-level `<plan_spec>` (framing, `HIGH` priority) + one `<task id status>` per generator task (summary-only evidence) + `<evaluation_criteria>` (authority, highest priority / last dropped). No `<goal>`, `<iteration_goal>`, prior-iteration background, failed-prior attempts, `<deferred_goal_for_next_iteration>`, or `<iteration>`/`<attempt>` wrapper.
3. **Contract de-duplication.** The shared role contract stops enumerating the tag set; the dynamic outline is the single authority.
4. **Cleanups** per A3 (and A5 optional). A4 is largely retired by E4.

---

## Work Breakdown (separately landable)

### Unit 1 — Evaluator recipe reduction to flat current-attempt (E4) — *highest value*
**Edits:**
- `recipes/evaluator.py`: remove the `goal_iteration_blocks(...)` **and** `failed_attempt_blocks(...)` calls; build the packet from the new flat current-attempt emitter only. Drop the now-unused `goal` fetch and `goal_id` from `required_scope_fields` (iteration derives from `attempt.iteration_id`; `ContextRefs.goal_id` is optional, `packet.py:47`).
- `recipes/attempts.py`: add a flat current-attempt emitter (evaluator-only) that produces **top-level blocks instead of one wrapped `pre_rendered_xml` `<attempt>` block**:
  - a `<plan_spec>` block built **fresh** from `attempt.plan_spec` (generator-style: `text=attempt.plan_spec`, `metadata={"tag":"plan_spec"}`, `HIGH` priority — `generator.py:59-69`), **not** via `_render_plan_spec_children`, which would re-append the deferred goal and the `"(not submitted)"` fallback;
  - one `<task id="…" status="…">` block per generator outcome, body = **summary only** (no task_spec), reusing `_render_task_element`'s summary logic (`attempts.py:227-231`); `status` stays on the tag;
  - an `<evaluation_criteria>` block (`_render_evaluation_criteria`, `attempts.py:234-238`) at the highest priority so it is the last block dropped under token budget — it is the authority;
  - **no `<deferred_goal_for_next_iteration>` block** — excluded by design (see ADR).
  Re-home `_sanitize_user_text` onto the new blocks, or keep them `pre_rendered_xml` with the existing guard (`attempts.py:293-302`). Leave `failed_attempt_blocks`, the wrapped `current_attempt_block`, and the shared `_render_plan_spec_children` **untouched** — the planner still uses `failed_attempt_blocks`, and its failed-prior blocks legitimately render `<deferred_goal_for_next_iteration>` as retry context via `_render_plan_spec_children`.
- `context_engine/tag_dictionary.py`: confirm/add `TAG_DICTIONARY` entries for `plan_spec`, `task`, `evaluation_criteria` so the "What's in context" outline renders them (unknown tags are silently dropped, `what_in_context.py:84-86`).
- `backend/config/skills/evaluator/SKILL.md`: **delete** `## Honor the iteration scope` (cites `<iteration_goal>` + `<iteration status="prior">`, lines 27-35) and **delete** `## Deferred-attempt handling` (lines 37-46) — the evaluator no longer receives the goal, iteration scope, prior attempts, *or* the deferred goal, so both sections describe absent blocks. Keep `## Use the criteria as authority`, `## Pick the right terminal`, `## Output discipline`; ensure retained text references only `<plan_spec>`, `<task>`, `<evaluation_criteria>`. The "no out-of-scope penalty" intent already lives in `## Use the criteria as authority` ("a met criterion is met even if a related-but-unstated outcome is missing"), so deleting the deferral section loses nothing.
- `agents/profile/main/evaluator.md`: update lines 25-28 — the body says the blocks "appear inside the `<attempt status="current">` body"; under E4 they are **top-level blocks**, so drop that clause.
- `_main_role_contract.md` (A2, ships with this unit): replace the explicit tag enumeration on line 3 with a pointer to the `<Task Guidance>` outline.

**Acceptance criteria:**
- U1-AC1: Evaluator packet contains *only* top-level `<plan_spec>`, `<task>`×N, and `<evaluation_criteria>` — no `<goal>`, `<iteration_goal>`, `<iteration>`/`<attempt>` wrapper, failed-prior blocks, or `<deferred_goal_for_next_iteration>` (verify the deferred-goal block is absent **even when the attempt was a defers-goal plan**).
- U1-AC2: Planner packet is byte-identical to before — `goal_iteration_blocks`, `failed_attempt_blocks`, and `_render_plan_spec_children` untouched, so the planner's failed-prior blocks still render `<deferred_goal_for_next_iteration>`. Snapshot/characterization.
- U1-AC3: `<plan_spec>` body is `attempt.plan_spec` verbatim, built fresh (not via `_render_plan_spec_children`, so no deferred-goal child sneaks in); `<task>` body is the generator **summary only** (no task_spec) with `status` on the tag.
- U1-AC4: No skill/profile/contract text instructs the evaluator to read a block it no longer receives — SKILL.md `## Honor the iteration scope` and `## Deferred-attempt handling` are **deleted**, the `evaluator.md` body clause is fixed, and the A2 contract enumeration is softened.
- U1-AC5: `goal_id` removed from the evaluator recipe's required scope fields; the packet still builds with `iteration` derived from `attempt.iteration_id`.
- U1-AC6: The "What's in context" outline lists the evaluator's top-level tags (`tag_dictionary` covers `plan_spec`/`task`/`evaluation_criteria`).

### Unit 2 — Explicit row structure (Item 1) — *behavior-preserving, leaned*
Item 1 is explicit user scope, kept in the leanest behavior-preserving form — no new abstraction.
**Edits:**
- `attempt/launch.py:126-151`: replace the 3-way branch with an inline declarative build —
  ```python
  # Canonical initial-message order: [system, context, guidance, skill?].
  # system is the agent_def system prompt; the rest are user rows; last → runner_prompt.
  rows = [r for r in (launch.context, launch.task_guidance, launch.skill) if r]
  if rows:
      runner_prompt = rows[-1]
      runner_initial_messages = [Message.from_user_text(r) for r in rows[:-1]] or None
  else:
      runner_prompt, runner_initial_messages = launch.context, None
  ```

**Acceptance criteria:**
- U2-AC1: For all three shapes (skill+guidance / guidance-only / neither) the final provider transcript is byte-identical to current behavior (characterization test before/after).
- U2-AC2: The canonical order is expressed declaratively in one readable statement; no 3-way branch remains.

**Alternative:** a named `AgentEntryMessages.ordered_user_rows()` method — rejected by default (indirection re-deriving an order the test already pins), low-cost if the self-documenting name is wanted.

### Unit 3 — Landscape cleanups (Item 3)
**A2 ships with Unit 1.** A3 (remove dead `advisor` directive) is trivial/independent. A5 optional polish. A4 largely retired by E4. A1 only if E2. A6/A7 are notes.

**Sequencing:** (1) **Unit 1 + A2** (the correctness reduction + coupled doc/skill fixes); (2) **A3** standalone; (3) **Unit 2** leaned, last.

---

## Test Surface (budgeted explicitly)
- **Unit 1 (E4 invalidates more than E1).** In `tests/unit_test/test_task_center/test_context_engine/test_recipes_other.py`: `test_evaluator_iteration2_frame_then_current_attempt` (~431-471) and any test asserting the `<iteration>`/`<attempt>` wrapper, prior-iteration frame, or failed-prior presence in the evaluator packet all lose their premise → **delete or rewrite to the flat shape**, not patch. Update assertions near lines 277-282, 458-469, and the `test_evaluator_with_empty_criteria_omits_criteria_block` tail (~474+). Fix the stale comment header (~line 241). Grep anchors: `build_evaluator_context`, `goal_iteration_blocks`, `failed_attempt_blocks`, `current_attempt_block`, `_render_plan_spec_children`. Add a snapshot of the new flat evaluator packet (U1-AC1) and a planner-unchanged snapshot (U1-AC2). **Asymmetry guard:** one test that a *defers-goal* attempt's evaluator packet contains **no** `<deferred_goal_for_next_iteration>`, paired with one that a planner retrying after a *failed* defers-goal prior **still** renders it in the failed-prior block.
- **Unit 2:** characterization test of the ordered message list for each launch shape, before/after (U2-AC1).
- **Unit 3:** A3/A2 are removals/prose — run the suite + any doc lint.
- **Runner:** `.venv/bin/pytest` (never global pytest — pytest-asyncio only loads in the uv venv). Targets: `test_context_engine`, `test_task_center`, `test_agents`.

## Risks
- **R1 (E4 drops prior-iteration background AND failed priors):** the evaluator loses all cross-attempt/iteration awareness. *Mitigation:* criteria + plan_spec are the authoritative contract; failed priors are planner retry-fuel; the skill already calls prior iterations "background." If regression awareness is ever shown necessary → fall back to **E3** (re-add failed priors). If only the structural refactor is too costly now → ship **E1** (same content, wrapped).
- **R2 (skill text):** we now **delete** `## Honor the iteration scope` and `## Deferred-attempt handling` rather than rewrite them (the evaluator receives none of those blocks), which lowers drift risk. *Mitigation:* verify the retained sections reference only `<plan_spec>`/`<task>`/`<evaluation_criteria>`; the "no out-of-scope penalty" intent survives in `## Use the criteria as authority`.
- **R3 (launch.py empty-rows edge):** `rows[-1]` on `[]` raises. *Mitigation:* explicit guard (shown) + U2-AC1 characterization test.
- **R4 (parallel agent activity / dirty worktree):** expected. *Mitigation:* scope edits to named files; stage explicit paths only.
- **R5 (test invalidation):** Unit 1 invalidates the evaluator iteration-frame/attempt-wrapper tests. *Mitigation:* delete/rewrite as part of the same PR; do not treat as a regression to "fix back."
- **R6 (E4 emitter decomposition — the new risk vs E1):** splitting the pre-rendered attempt body into top-level blocks must preserve structural-closer sanitization and add `tag_dictionary` entries, or the outline silently drops the new tags (A7). *Mitigation:* re-home `_sanitize_user_text` per block or keep `pre_rendered_xml` with the existing guard; assert U1-AC6. This is the part of E4 not covered by the original E1 consensus — review it explicitly (or re-run the Critic on Unit 1) before merge.

---

## ADR

**Decision.** Reduce the evaluator context to the **current attempt, flat** (**E4**): drop the shared `goal_iteration_blocks` and `failed_attempt_blocks` calls from `recipes/evaluator.py`, and emit the current attempt's substance as top-level blocks — `<plan_spec>` (built fresh from `attempt.plan_spec`, not via `_render_plan_spec_children`) + one `<task id status>` per generator task (summary-only) + `<evaluation_criteria>` — with **no** `<iteration>`/`<attempt>` wrapper and **no** `<deferred_goal_for_next_iteration>`. Ship the coupled `_main_role_contract.md` de-enumeration (A2), the evaluator skill **section deletions** (`## Honor the iteration scope`, `## Deferred-attempt handling`), and the `evaluator.md` body fix in the same unit. Make the `[system, context, guidance, skill?]` order explicit via an inline declarative list in `launch.py` (Unit 2). Remove the dead `ROLE_DIRECTIVES["advisor"]` key (A3).

**Drivers.** (1) Defer-path correctness — a binary evaluator must not be *able* to weigh the full goal against a deliberately-bounded slice. (2) Criteria-as-authority / locality — the evaluator judges this attempt against these criteria; anything else is planner-scope or retry-fuel. (3) Minimal blast radius — the planner's context stays byte-identical; the refactored current-attempt emitter is evaluator-only.

**Alternatives considered.** *E1* (same content, keeps `<iteration>`/`<attempt>` wrappers; pure deletion) — retained as the minimal-diff fallback if the flat refactor is deferred. *E3* (keep failed priors) — rejected by default; re-adopt only if regression awareness is shown necessary. *E2* (keep prior-iteration background; needs helper split) — rejected. *`<task>` = task_spec + summary* — rejected: task_specs are the generator/verifier contract; the evaluator judges at the criteria level and uses its own read/shell tools for ground truth. *Keep `<deferred_goal_for_next_iteration>` in the evaluator (E4 + deferred)* — rejected: it re-creates the defer-path incompleteness trap and is redundant with the already-slice-scoped criteria; the continuation reads it from attempt state regardless. *Unit 2 as a named method* — rejected as indirection.

**Why chosen.** Structural removal makes misweighing *unrepresentable* — stronger than the skill-prose guardrail that exists today. `<deferred_goal_for_next_iteration>` is excluded for the same reason as `<goal>`: it names the out-of-scope remainder and would re-create the incompleteness-bias trap; it is a lifecycle artifact read from attempt *state* to spawn the continuation iteration, not evaluation evidence (and the criteria of a defers-goal plan are already scoped to the slice). Verified downstream-safe (no consumer reads evaluator output against the goal). The flat shape makes `<evaluation_criteria>` the most prominent / last-dropped block and matches the generator recipe's locality. Zero planner impact — the planner still sees the deferred goal in its failed-prior blocks via the untouched `_render_plan_spec_children`.

**Consequences.** Evaluator loses cross-attempt/iteration awareness *and* the deferred-goal block (accepted; criteria + plan_spec are the contract; E1/E3 are named fallbacks). The shared contract stops enumerating tags (drift surface retired). The `FAILED_ATTEMPT` enum overload is naturally retired on the evaluator path (A4). The evaluator skill's `## Honor the iteration scope` and `## Deferred-attempt handling` sections are **deleted** (not rewritten). A set of evaluator iteration-frame/attempt-wrapper tests are invalidated and must be deleted/rewritten (R5). The current-attempt emitter is refactored from one pre-rendered block into top-level blocks (with `<plan_spec>` built fresh, not via the shared helper), carrying the new R6 (sanitization + outline coverage) — the one part of E4 beyond the original E1 consensus. The evaluator/planner deferred-goal asymmetry is intentional: planner-relevant retry context, evaluator-irrelevant evidence.

**Follow-ups (optional, separately landable).** A5 (single-source the directive↔dispatch binding), A6 (verifier-without-skill), A4 rename `FAILED_ATTEMPT`→`ATTEMPT`, A1 (split `goal_iteration_blocks` — only if E2 is ever adopted). A7 is a standing coupling note (renderer ↔ outline ↔ `tag_dictionary`).

---

## Implementation Status (2026-05-29)

### Landed
- **Unit 1 — Evaluator flat current-attempt (E4).**
  - `recipes/attempts.py`: replaced `current_attempt_block` / `_render_current_attempt_body` with `current_attempt_flat_blocks(*, attempt, task_store)` — top-level `<plan_spec>` (HIGH, built fresh from `attempt.plan_spec`) + one `<task id status>` per generator outcome (HIGH, summary-only via `_task_outcome_block`) + `<evaluation_criteria>` (REQUIRED, last dropped). No `<iteration>`/`<attempt>` wrapper, no `<deferred_goal_for_next_iteration>`. Per-field sanitization delegated to the renderer's structural-closer guard (ordinary, non-`pre_rendered_xml` blocks). Module docstring rewritten; `failed_attempt_blocks` (now planner-only) and `_render_plan_spec_children` left untouched.
  - `recipes/evaluator.py`: builds the packet from the flat emitter only; dropped `goal_iteration_blocks` + `failed_attempt_blocks`; `required_scope_fields = {attempt_id}`; iteration derived from `attempt.iteration_id`. (Judgment call: kept `canonical_refs.goal_id = scope.goal_id` for persisted-packet provenance — harmless, no goal fetch; the plan permitted dropping it.)
  - Docs (ship with Unit 1): `_main_role_contract.md` de-enumerated (A2); `evaluator.md` dropped the "appear inside the `<attempt status="current">` body" clause; evaluator `SKILL.md` deleted `## Honor the iteration scope` + `## Deferred-attempt handling`, reworded the out-of-scope bullet to reference "stated criteria", and refreshed the frontmatter description.
- **Unit 2 — Explicit row order.** `attempt/launch.py` replaced the 3-way branch with the declarative `[system, context, guidance, skill?]` build (`rows = [r for r in (context, task_guidance, skill) if r]`).
- **Unit 3 / A3.** Removed the dead `ROLE_DIRECTIVES["advisor"]` key (advisor bypasses the composer).
- **Dead-code removal (user step 2, beyond the plan's "leave untouched" Unit-1 conservatism):** deleted the now-orphaned `current_attempt_block` / `_render_current_attempt_body` and their dedicated tests rather than leaving them dead.
- **Architecture memory refresh:** corrected the 4 stale evaluator claims in `docs/architecture/task_center/context-engine.html` (scope card, role-recipe table row, current-attempt trace, evaluator-frame deferred-goal note).

### Tests (rewritten/added, all green)
- `test_recipes_other.py` (evaluator section → flat shape; added the **defers-goal ⇒ no deferred block** asymmetry guard, a no-goal/iteration-scope build test for U1-AC5, and an empty-packet-without-plan_spec test).
- `test_attempts.py` (`current_attempt_block` tests → `current_attempt_flat_blocks`).
- `test_iteration_no_invariant.py` (removed the obsolete current-attempt tests + unused `_FakeAttempt` / `ContextPriority` import).
- `test_context_engine/test_task_guidance.py` (the relocated `test_builders.py`) — evaluator outline test → flat.
- `test_attempt_launcher_retry.py` — added the skill+guidance 4-row characterization test (U2-AC1).
- `test_role_directives.py` — dropped the `advisor` expected entry.
- **Run:** `test_context_engine` (139), `test_task_center` (346), `test_agents` (55), the two contract tests in `test_engine` (7) — all pass; `ruff check` clean on changed sources. U1-AC2 (planner byte-identical) is verified by the unchanged `test_recipes_planner_closes_or_defers.py` + `test_attempts.py::test_prior_attempt_body_emits_deferred_goal_when_present` staying green.

### Concurrency note
A parallel agent landed a module-rename refactor during this work (`what_in_context.py`→`context_outline.py` with `render_what_in_context`→`render_context_outline`; `task_guidance/builders.py`→`context_engine/task_guidance.py`; `task_state.py`→`_core/task_state.py`; the `test_builders.py`→`test_task_guidance.py` move). My edits were integrated against the relocated paths; none of my recipe changes depend on the renamed modules.

### Deferred (not done — see ADR "Follow-ups" above)
- **A1** — split `goal_iteration_blocks` into primitives. *Only needed if E2 is ever adopted; E4 simply stopped calling it.*
- **A4** — rename `ContextBlockKind.FAILED_ATTEMPT`→`ATTEMPT`. *Overload is retired on the evaluator path by E4; the planner still uses the kind, so the rename is cosmetic.*
- **A5** — single-source the `ROLE_DIRECTIVES` ↔ `_AGENTS_WITH_TASK_GUIDANCE` binding (or add startup validation). *`build_task_guidance` still `KeyError`s if a dispatched role lacks a directive.*
- **A6** — `verifier` shares the `generator` recipe but declares no skill. *Note only — out of scope unless a verifier skill is wanted.*
- **A7** — standing coupling note: renderer ↔ outline (`context_outline`) ↔ `tag_dictionary` must stay in lockstep; an unknown tag is silently dropped from the outline.
