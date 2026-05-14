# Agent Profile Router — Consensus Replan (v7, APPROVED)

Produced via `ralplan` consensus loop (Planner → Architect → Critic ×3) on 2026-05-14.
User: replan agent profiles for each role; build a sophisticated rule-based router; resolve "agent" vs "role" naming confusion.

---

## RALPLAN-DR Summary

### Principles
- **P1 — Surgical naming.** Fix one root: rename `AgentDefinition.role` → `AgentDefinition.agent_kind: AgentKind`. The `agent_kind` field name (not `kind`) avoids collision with the existing `AgentSelectionBlock.kind: str` discriminator. `HarnessTaskRole` (dispatcher's lifecycle bucket) is untouched.
- **P2 — Frontmatter is the routing contract.** Extend the existing `RuleBasedAgentResolver` with new predicates rather than introducing a parallel router engine. The "sophisticated router" the user wants already exists; new behavior comes from new predicates, not a new engine.
- **P3 — Variant partitions are mechanically total.** Every variants-having profile that has empty terminals MUST end its variants list with `when: always`. Every variants-having profile with `len(variants) > 1` MUST end with `when: always`. These two lint rules close the silent no-terminal class of bugs.
- **P4 — Post-entry shallow handoff, deep stop.** For non-entry executors, depth ∈ [0,2] equips success + handoff (no failure terminal); depth ≥3 equips success + failure (no handoff). The entry executor is a documented exception that retains all three terminals because it terminates the user-facing request.
- **P5 — No silent breakage.** Every persisted role string and every "executor"/"verifier"/"generator" hardcoding is enumerated and migrated atomically. Stage 1 and Stage 4 ship as single coordinated commits.

### Decision drivers
- **D1.** Naming clarity is half the work — user's own framing flips between "agent" and "role".
- **D2.** Depth must be unambiguously computable from data the resolver already has access to (mission ancestry chain).
- **D3.** Migration safety — existing `agent_name="executor"` task rows must resolve via the new variants without a DB rewrite.

### Viable options
**Option A — Extend variants + new predicates (CHOSEN).** Pros: zero new resolver code; reuses existing `validate_agent_definitions_resolved`; matches existing pattern. Cons: depth thresholds baked into predicate identifiers; routing rules co-located with profiles. Mitigation: extract `MAX_HANDOFF_DEPTH = 2` as a module constant; predicate names use `_within_handoff_range` / `_above_handoff_range` (range-named) instead of integer-named.

**Option B — Centralized router (`routing.yaml`).** Pros: single grep-able policy; threshold is data not identifier. Cons: duplicates variant validator; depth computation still lives in Python (predicates would need the same helpers); weakens P2; new validator code; `validate_agent_definitions_resolved` machinery would be duplicated. **Rejected** because the `MAX_HANDOFF_DEPTH` constant gives Option A the same tunability without splitting the source of truth.

**Option C — Hybrid.** Strictly dominated by A or B. **Rejected.**

---

## Naming proposal

| Concept | Today | After this plan |
|---|---|---|
| Dispatcher lifecycle bucket | `HarnessTaskRole` (PLANNER / GENERATOR / EVALUATOR) | unchanged — dispatcher detail |
| Human-facing agent category | `AgentDefinition.role: str \| None` (free-form) | `AgentDefinition.agent_kind: AgentKind` (enum, required) |
| Concrete loaded definition | `AgentDefinition.name` (file stem) | unchanged — call this "agent profile" in docs |
| Selected at launch | `AgentVariant` (`when` predicate + `use` target) | unchanged |
| Tool-metadata key emitted by `factory.py` and `run_subagent.py` | `"role"` carrying free-form string | `"role"` carrying `agent_def.agent_kind.value` (key kept for audit-consumer compat) |

Drop "generator" from human-facing vocabulary; keep only as `HarnessTaskRole.GENERATOR` (a dispatcher detail).

### `AgentKind` enum (7 values)
```python
class AgentKind(StrEnum):
    PLANNER = "planner"
    EXECUTOR = "executor"
    VERIFIER = "verifier"
    EVALUATOR = "evaluator"
    ADVISOR = "advisor"      # helper kind
    EXPLORER = "explorer"    # subagent kind
    RESOLVER = "resolver"    # helper kind
```

The 7 values mirror the existing `role:` strings byte-for-byte, so `agent_def.agent_kind.value == old agent_def.role` for every registered profile. No state migration. The depth-based variant predicates apply only to the 4 main kinds; helper/subagent profiles never declare `variants:`.

### `dispatchable_by_planner: bool` field

A new explicit flag on `AgentDefinition`, default `False`. Set `True` only on profiles a planner may legitimately submit as a generator task. After this plan lands:

| Profile | `dispatchable_by_planner` | Rationale |
|---|---|---|
| `executor` (thin entry-point) | **`True`** | The name planners submit; resolver picks the depth variant |
| `verifier` (`generator_verifier.md`) | **`True`** | The name planners submit |
| `executor_success_handoff` | `False` | Selected by resolver; not a planner-submittable name |
| `executor_success_failure` | `False` | Same |
| `entry_executor` | `False` | Top-of-stack only — never planner-submittable; closes the bug the advisor caught |
| `planner` / `planner_full_only` / `evaluator` | `False` | Not generator-capable |
| `resolver` / `advisor` / `explorer` | `False` | Helper / subagent — dispatched via `ask_*` / `run_subagent`, never via plan |

`_is_generator_capable_agent` (Stage 6) checks **both** `definition.dispatchable_by_planner is True AND definition.agent_kind in {EXECUTOR, VERIFIER}` — defense in depth. Single-source-of-truth alternative was rejected because the advisor specifically flagged the entry_executor hole as a bug class (`agent_kind: executor` was previously masked by a literal-name fast-path).

---

## Acceptance criteria

| AC | Statement | Verification |
|---|---|---|
| AC1 | Adding a new routing rule requires only (a) profile MD, (b) predicate function with one-line `register_builtin_predicates` call, (c) `variants:` entry. Resolver code untouched. | Repo grep: PR adding a new rule touches no files in `task_center/agent_launch/resolver.py`. |
| AC2 | `_is_generator_capable_agent` checks `dispatchable_by_planner is True AND agent_kind ∈ {EXECUTOR, VERIFIER}`; literal-name fast-path deleted. | Tests: (a) register `executor_alt` with `agent_kind=EXECUTOR, dispatchable_by_planner=True` → accepted. (b) register with `agent_kind=EXECUTOR, dispatchable_by_planner=False` → rejected. (c) `_is_generator_capable_agent("entry_executor")` → False (regression test for the advisor-caught bug). |
| AC3 | Depth 0–2 non-entry executor sees `[submit_execution_success, submit_execution_handoff]`; depth ≥3 sees `[submit_execution_success, submit_execution_failure]`. | Integration test composes context for executor at depths 0/1/2/3; asserts on the resolved `agent_def.terminals`. |
| AC4 | Depth-1 planner sees full + partial; depth ≥2 planner sees full only. | Same as AC3 for planner profiles. |
| AC5 | `_partial_plan_caller_ancestor` predicate and `has_partial_planned_caller_ancestor` helper are deleted in the same commit as the new predicates land. | grep -r confirms zero references to either symbol after Stage 2. |
| AC6 | Existing tests pass; lockstep test updates land in their respective stage commits (Stage 1 ~17 files for kind rename; Stage 4 ~25 files for tool rename). | CI green at end of each stage commit. |
| AC7 | Resolver coverage invariant — for thin-base-with-no-terminals profiles (currently only `executor.md`), the variants list disjunction evaluates True for every depth in {0,1,2,3,4}. | Unit test enumerates depths and asserts each `executor.md` variant disjunction is True. |
| AC8 | The `always` predicate is registered and returns True on any `ResolverContext`. | Direct unit test. |
| AC9 | `validate_agent_definitions_resolved` raises `AgentDefinitionValidationError` on (a) `len(variants) > 1` without a final `when: always` entry, OR (b) `variants:` non-empty AND `terminals:` empty without a final `when: always` entry. The "final element" requirement is explicit. | Unit tests for both rules; a passing case (planner.md: 1 variant + non-empty terminals) and 4 failing cases. |
| AC10 | Audit-shape regression: `metadata["role"] == agent_def.agent_kind.value` for every emitted audit entry — exact equality, not set membership. | Regression test reads sample audit JSON. |

---

## Implementation stages

### Stage 1 — Naming refactor (`role` → `agent_kind`)

**Source code (in scope):**
1. `backend/src/agents/definition/model.py` — add `AgentKind` enum (7 values), replace `role: str | None` with `agent_kind: AgentKind` (required), AND add `dispatchable_by_planner: bool = False` field. Profile MD frontmatter that should be planner-submittable sets `dispatchable_by_planner: true` explicitly: today only `executor.md` (the thin entry-point after Stage 3) and `generator_verifier.md`. All other profiles inherit the `False` default — including `entry_executor.md`, which closes the advisor-flagged bug.
2. `backend/src/agents/profile/main/planner.md` — frontmatter `role:` → `agent_kind:` (planner).
3. `backend/src/agents/profile/main/planner_full_only.md` — same.
4. `backend/src/agents/profile/main/evaluator.md` — same (evaluator).
5. `backend/src/agents/profile/main/generator_executor.md` — same (executor); also restructured by Stage 3.
6. `backend/src/agents/profile/main/generator_verifier.md` — same (verifier).
7. `backend/src/agents/profile/main/entry_executor.md` — same.
8. `backend/src/agents/profile/helper/resolver.md` — same (resolver).
9. `backend/src/agents/profile/helper/advisor.md` — same (advisor).
10. `backend/src/agents/profile/subagent/explorer.md` — same (explorer).
11. `backend/src/tools/submission/planner/_schemas.py:79-83` — kind-only check (also Stage 6).
12. `backend/src/tools/subagent/run_subagent.py:201-202` — `sub_def.role` → `sub_def.agent_kind.value`. Drop the truthy guard (`if sub_def.role:` no longer needed since `agent_kind` is required).
13. `backend/src/engine/agent/factory.py:195, 347` — `agent_def.role` → `agent_def.agent_kind.value`. Drop any `or ""` fallbacks.
14. `backend/src/live_e2e/squad/runner.py` — 14 references at lines **3 (docstring)**, 190, 210, 212, 214, 216, 232, 234, 236, 238, 241, 270, 1177, 1437.

**Test files (lockstep):**
15. `backend/tests/unit_test/test_agents/test_planner_full_only_md.py:23-24` — `planner.role == "planner"` → `planner.agent_kind == AgentKind.PLANNER`.
16. `backend/tests/unit_test/test_agents/test_agent_markdown.py:43` — same pattern.
17. Any other `tests/**/*.py` matching `\.role` against `AgentDefinition` instances — full-grep done in lockstep with implementation; results pasted into PR description.

**Out of scope (verified):**
- `backend/src/live_e2e/scenarios/full_case_user_input.py:96`, `full_stack_adversarial.py:120`, `hooks/builtins.py:124` — `role=` is a kwarg on `inject_failure(role=...)` / `capture_prompt(role=...)` (string keying into `_ROLE_TO_INVOKED`), not `AgentDefinition.role`.
- `backend/src/live_e2e/audit/recorder.py:39, 473, 484` — `TaskCenterTaskRecord.role` (persisted task-role string), not `AgentDefinition.role`.
- `backend/src/db/stores/task_center_store.py:236, 257` — same.
- `backend/src/agents/definition/loader.py` — generic frontmatter parser; Pydantic auto-handles the field rename when frontmatter changes.

### Stage 2 — Depth helper + new predicates + DELETE old predicate (one commit)

- `backend/src/task_center/mission/ancestry.py`:
  - Add `nested_mission_depth(*, mission_id, mission_store, episode_store, attempt_store, task_store) -> int`. Walks `parent_task → parent_attempt → parent_episode → parent_mission` (mirror of the existing helper) and returns the number of mission ancestors INCLUDING `mission_id`. For `mission_id=None` (entry executor), the resolver short-circuits to depth 0 before calling.
  - **Delete** `has_partial_planned_caller_ancestor` in the same commit.
- `backend/src/task_center/agent_launch/predicates.py`:
  - Add `MAX_HANDOFF_DEPTH = 2` module constant.
  - Add 4 new boolean predicates:
    - `_nested_mission_depth_within_handoff_range(ctx) -> bool: return depth(ctx) <= MAX_HANDOFF_DEPTH`
    - `_nested_mission_depth_above_handoff_range(ctx) -> bool: return depth(ctx) > MAX_HANDOFF_DEPTH`
    - `_nested_mission_depth_gt_1(ctx) -> bool: return depth(ctx) > 1`
    - `_always(ctx) -> bool: return True`
  - **Delete** `_partial_plan_caller_ancestor` in the same commit.
  - `register_builtin_predicates` registers the 4 new predicates and removes the old registration.

**Test files (lockstep — required by AC5 grep guard):**
- `backend/tests/unit_test/test_agents/test_planner_full_only_md.py:56` — variant predicate assertion `partial_plan_caller_ancestor` → `nested_mission_depth_gt_1`.
- `backend/tests/unit_test/test_agents/test_registry_validation.py:134, 141` — registry test fixture uses old predicate name.
- `backend/tests/unit_test/test_agents/test_definition_variants.py:21` — variant fixture uses old predicate name.
- `backend/tests/unit_test/test_task_center/test_orchestrator_composer.py:110` — same.
- `backend/tests/unit_test/test_task_center/test_domain/test_ancestry.py:16, 110, 133, 164, 196, 245, 259, 313, 314` — direct references to `has_partial_planned_caller_ancestor`. File either retargets to `nested_mission_depth` semantics or is deleted/replaced with `test_nested_mission_depth.py` per Stage 2's helper rename.

### Stage 3 — Profile splits

**`backend/src/agents/profile/main/executor.md`** (renamed from `generator_executor.md`): thin entry-point.
```yaml
name: executor
agent_kind: executor
context_recipe: generator_v1
variants:
  - when: nested_mission_depth_above_handoff_range
    use: executor_success_failure
    note: "depth >2 — leaf executor, no further handoff"
  - when: always
    use: executor_success_handoff
    note: "depth ≤2 — handoff allowed"
```
No terminals, no allowed_tools, no body — they live on the variant targets. Planner submits `agent_name="executor"`; resolver picks the variant.

**`backend/src/agents/profile/main/executor_success_handoff.md`** (NEW):
```yaml
name: executor_success_handoff
description: Generator executor — depth-shallow profile (success + handoff, no failure terminal).
agent_kind: executor
allowed_tools: [read_file, write_file, edit_file, shell, run_subagent, ask_advisor]
terminals: [submit_execution_success, submit_execution_handoff]
notification_triggers: [request_mission_after_edit]
context_recipe: generator_v1
```
Body explains success-or-handoff dichotomy; notes that abandoning ends in launcher-synthesised run-exhausted failure (per `launcher.py:283-301`).

**`backend/src/agents/profile/main/executor_success_failure.md`** (NEW):
```yaml
name: executor_success_failure
description: Generator executor — depth-deep profile (success + failure, no further handoff).
agent_kind: executor
allowed_tools: [read_file, write_file, edit_file, shell, run_subagent, ask_advisor]
terminals: [submit_execution_success, submit_execution_failure]
notification_triggers: []
context_recipe: generator_v1
```
Body explains leaf-executor — explicit failure is the escape valve.

**`backend/src/agents/profile/main/entry_executor.md`** (modified):
- Rename terminal `request_mission_solution` → `submit_execution_handoff`.
- Add `agent_kind: executor`.
- **Keep all three terminals** (success / handoff / failure) — documented carve-out per P4. Body adds a one-paragraph explainer for the exception with cross-ref to the wiki.

**`backend/src/agents/profile/main/planner.md`** (modified):
- Replace variant predicate `partial_plan_caller_ancestor` → `nested_mission_depth_gt_1`.
- Add `agent_kind: planner`.
- Keep terminals `[submit_full_plan, submit_partial_plan]` (non-empty); single variant entry; AC9 lint passes (1 variant + non-empty terminals).

**`backend/src/agents/profile/main/planner_full_only.md`, `evaluator.md`, `generator_verifier.md`** (modified): add `agent_kind:` field; otherwise unchanged. (`generator_verifier.md` keeps `name: verifier`.)

### Stage 4 — Tool rename (`request_mission_solution` → `submit_execution_handoff`)

Atomic 25-file commit:

**Production (move + import updates):**
1. `backend/src/tools/submission/executor/request_mission_solution.py` — MOVE to `submit_execution_handoff.py`; rename function + `@tool(name=...)`. Input schema unchanged.
2. `backend/src/tools/submission/executor/__init__.py` — update export.
3. `backend/src/tools/submission/_factory.py` — update import + factory list.
4. `backend/src/tools/submission/notification_triggers/request_mission_after_edit.py:32` — update body string.

**Profile MDs:**
5. `backend/src/agents/profile/main/entry_executor.md` — terminal rename (also covered by Stage 3).
6. `backend/src/agents/profile/main/generator_executor.md` (now `executor.md`) — after Stage 3, this file has NO `terminals:` field; deterministic check: `grep -L 'request_mission_solution' agents/profile/main/executor.md` matches.
7. `backend/src/agents/profile/main/executor_success_handoff.md` (NEW from Stage 3) — uses new tool name from creation.

**Production callsites referencing the tool name string:**
8. `backend/src/task_center/entry/controller.py`
9. `backend/src/task_center/entry/coordinator.py`
10. `backend/src/task_center/mission/starter.py`
11. `backend/src/db/models/mission.py`
12. `backend/src/live_e2e/squad/runner.py:52-53, 284, 363, 385`
13. `backend/src/live_e2e/scenarios/tools/__init__.py`

**Tests (lockstep):**
14. `backend/tests/unit_test/test_agents/test_agent_markdown.py:44, 50`
15. `backend/tests/unit_test/test_benchmarks/test_sweevo_mock_agent_execution.py:230`
16. `backend/tests/unit_test/test_task_center/conftest.py:170`
17. `backend/tests/unit_test/test_task_center/test_lifecycle/test_mission_handler.py`
18. `backend/tests/unit_test/test_tools/conftest.py:144`
19. `backend/tests/unit_test/test_tools/test_submission_soft_reminders.py:72`
20. `backend/tests/unit_test/test_tools/test_submission_terminal_routing.py:24, 175, 214, 259`
21. `backend/tests/unit_test/test_tools/test_submission_tool_registration.py:16, 49, 51`

**E2E:**
22. `backend/src/live_e2e/tests/sweevo/test_complex_project_build.py:213, 218`

**Docs:**
23. `docs/wiki/role-generator.md`
24. `docs/wiki/task-center-pipeline.md`
25. `docs/wiki/tools-hooks-guardrails-agents-notifications-messages.md`

**Atomic commit guard.** Stage 4 is one commit because partial application breaks `from tools.submission.executor import …` at any intermediate state. CI grep check after the commit: `! grep -rn "request_mission_solution" backend/ docs/wiki/ | grep -v -E "(\.git|\.planning)"`.

### Stage 5 — DELETED (merged into Stage 2)

### Stage 6 — Schema check + lint rule

- `backend/src/tools/submission/planner/_schemas.py:79-83`:
  ```python
  def _is_generator_capable_agent(agent_name: str) -> bool:
      definition = get_definition(agent_name)
      if definition is None:
          return False
      return (
          definition.dispatchable_by_planner
          and definition.agent_kind in {AgentKind.EXECUTOR, AgentKind.VERIFIER}
      )
  ```
  Delete the literal-name fast-path entirely. The `dispatchable_by_planner` gate closes the entry_executor hole the advisor caught (entry_executor has `agent_kind: executor` but `dispatchable_by_planner: false`, so `_is_generator_capable_agent("entry_executor") → False`).
- `backend/src/agents/definition/resolved_validation.py`: extend `validate_agent_definitions_resolved` to enforce AC9 lint rules. Raise `AgentDefinitionValidationError` (existing exception type used at lines 31, 37, 43, 48, 53) when:
  - `len(definition.variants) > 1` AND the FINAL element does not have `when == "always"`.
  - `definition.variants` non-empty AND `definition.terminals` empty AND the FINAL element does not have `when == "always"`.

  The "final element" requirement is explicit (not "present anywhere"): first-match-wins semantics mean an `always` not in tail position would shadow subsequent entries.

### Stage 7 — Wiki refresh (overlaps with Stage 4 docs commit)

- `docs/wiki/role-generator.md` → retitle to `agent-kinds-and-profiles.md` (or split into `agent-executor.md` + `agent-verifier.md`):
  - Replace "role" with "agent kind" where applicable.
  - Document the new depth-gated profile selection.
  - Remove `request_mission_solution` references.
  - Note `HarnessTaskRole.GENERATOR` is a dispatcher detail.
- `docs/wiki/role-evaluator.md`, `role-planner.md` — terminology refresh; mention `AgentKind`.
- New section: explain three concepts — `HarnessTaskRole` / `AgentKind` / agent profile.

---

## Test plan

**Unit:**
- `nested_mission_depth` table-driven (depths 0/1/2/3+; cycle detection mirrors `has_partial_planned_caller_ancestor` invariant).
- 4 new predicates with depth fixtures (`_within_handoff_range`, `_above_handoff_range`, `_gt_1`, `_always`).
- `RuleBasedAgentResolver.resolve` for `executor` at depth 0/1/2/3 → correct profile.
- `RuleBasedAgentResolver.resolve` for `planner` at depth 1/2/3 → correct profile.
- `_is_generator_capable_agent`: register temp `executor_alt` definition with `agent_kind=EXECUTOR`; assert validation accepts. Negative: `agent_kind=PLANNER` → reject.
- Each profile loads cleanly with `agent_kind` field; missing `agent_kind` raises `ValidationError`.
- AC7: enumerate depths {0,1,2,3,4}; assert `_within_handoff_range OR _above_handoff_range` evaluates True for each.
- AC8: `_always(ResolverContext(scope=any, deps=any))` is True for any input.
- AC9: 4 failing cases + 1 passing case (planner.md shape).
- AC10: regression test asserts `metadata["role"] == agent_def.agent_kind.value` (exact equality).

**E2E:**
- `test_partial_parent_planner_full_only.py` — update predicate name; assert behavior unchanged.
- SWE-EVO scenarios — verify depth measurement correct in 2-level and 3-level nested missions.

**Manual:**
- Render a sample tool palette for an executor at depth 0, 1, 2, 3 and a planner at depth 1, 2, 3 — sanity-check terminals.

---

## Pre-mortem (3 scenarios)

1. **Audit consumers break because `factory.py` emits a different metadata key value.**
   *Mitigation:* keep emitting key `"role"`; only the value source flips from `agent_def.role` to `agent_def.agent_kind.value`. AC10 regression test asserts exact equality. `AgentKind` string values mirror current `role` strings byte-for-byte (planner/executor/verifier/evaluator/advisor/explorer/resolver) → no value drift.

2. **Entry executor exception confuses future maintainers, who delete `submit_execution_failure` "for consistency".**
   *Mitigation:* P4 wording explicitly names the exception; `entry_executor.md` body has a paragraph explaining why; wiki cross-ref. Follow-up: consider a dedicated `AgentKind.ENTRY_EXECUTOR` to make the exception type-checked.

3. **A future engineer adds a third executor profile (e.g. `executor_high_risk`) without registering its predicate as a partition member, breaking total coverage.**
   *Mitigation:* AC9 lint rule + AC7 depth-enumeration test. Adding a new variant without an `always`-tail (when terminals empty) or an updated coverage test fails CI before merge.

---

## ADR

**Decision.** Keep `HarnessTaskRole` 3-way (planner/generator/evaluator) — it's the dispatcher's lifecycle bucket. Add `AgentKind` 7-way (planner/executor/verifier/evaluator/advisor/explorer/resolver) on `AgentDefinition.agent_kind`, replacing the overloaded `role`. Replace `partial_plan_caller_ancestor` predicate with `nested_mission_depth_gt_1`. Split the `executor` base profile into a thin variants-only entry-point with two leaf profiles (`executor_success_handoff` for depth ≤2, `executor_success_failure` for depth >2). Rename `request_mission_solution` → `submit_execution_handoff`. The `always` predicate enforces total coverage of variant partitions, formalized via a new lint rule in `validate_agent_definitions_resolved`.

**Drivers.** Naming clarity (D1); explicit topological invariant on handoff vs failure (P4); preserve "frontmatter is source of truth" for routing (P2); migration safety via mirror-string enum values (D3).

**Alternatives considered.**
- **`routing.yaml` central router (Option B).** Rejected because (a) the existing `validate_agent_definitions_resolved` enforcement path would be duplicated, (b) depth computation still lives in Python regardless, (c) frontmatter co-locates the rule with the profile that owns it, (d) the threshold-tunability concern raised by Architect §1 is solved by extracting `MAX_HANDOFF_DEPTH` as a module constant + range-named predicates instead of integer-named.
- **Splitting `HarnessTaskRole.GENERATOR` into EXECUTOR + VERIFIER (4-way structural enum).** Rejected because failure-handling for the two is identical (`launcher.py:325-332`); splitting forces duplicate `_report_X_exhaustion` paths with no benefit. Dispatcher genuinely doesn't care about the executor/verifier distinction; only the agent definition does.
- **Field name `kind` (instead of `agent_kind`).** Rejected — collides with `AgentSelectionBlock.kind: str` (`model.py:44`).
- **4-value `AgentKind` (planner/executor/verifier/evaluator only).** Rejected — would break `helper/resolver.md`, `helper/advisor.md`, `subagent/explorer.md` which today set `role: resolver|advisor|explorer`. Extending to 7 values mirrors the existing string set; helper/subagent profiles never declare variants so depth predicates never apply to them.
- **Keeping `role` field alongside `agent_kind` for one release cycle.** Rejected — only 17 callsites in src/ + tests, hard cutover is one commit.

**Why chosen.** Lowest blast radius that satisfies user goals. The "router engine" already exists in `RuleBasedAgentResolver`; new behavior comes from new predicates, not a new engine. Sophistication is achieved by composing more predicates per profile.

**Consequences.**
- Planner prompts unchanged — `tasks[{agent_name}]="executor"` keeps working; resolver picks the depth-specific variant at launch.
- Existing `agent_name="executor"` task rows in DB resolve naturally via the new variants on next dispatch.
- Audit consumers continue reading metadata key `"role"`; values continue matching the existing string set (AC10).
- Field name is `agent_kind` (not `kind`) to avoid collision with `AgentSelectionBlock.kind`. The two are unrelated concepts.
- The `always` predicate is now a registered building block; future profile authors may reuse it. Document semantics in the wiki.
- The thin-base-with-variants pattern (executor.md) is generalizable; future agent-kind splits follow the same shape.

**Follow-ups.**
- Consider whether `entry_executor` deserves its own `AgentKind.ENTRY_EXECUTOR` instead of being an `executor` exception.
- If/when the depth threshold becomes runtime-tunable, replace `MAX_HANDOFF_DEPTH` constant with a config injection.
- Once the dust settles, evaluate whether `helper`/`subagent` agent-types should be folded into `AgentKind` more cleanly (e.g., a separate `category: main|helper|subagent` field) — currently `agent_type` already carries this distinction.

---

## Consensus loop record

- **Iteration 1 (Architect):** Identified 6 fixes — entry-executor exception, AC1 overclaim, literal-name fast-path coupling, depth-monotonicity invariant, `routing.yaml` steelman, tool-metadata key decision.
- **Iteration 2 (Critic v3):** ITERATE — silent no-terminal executor (Major), AC7 untestable (Major), missed `run_subagent.py:201-202` callsite, Stage 5 ordering.
- **Iteration 3 (Critic v4):** ITERATE — Stage 4 missed 9+ test files, recorder.py:473 contradiction, runner.py count was 8 but actually 13, AC7 wording overreaches.
- **Iteration 4 (Critic v5):** ITERATE — `kind` collides with `AgentSelectionBlock.kind` (CRITICAL), AC9 wording risk, planner.md asymmetry edge case.
- **Iteration 5 (Critic v6):** ACCEPT-WITH-RESERVATIONS — confirmed `agent_kind` resolves collision; 3 minor cleanups (AC9 "final element" wording, exact-equality audit assertion, helper-profile role values).
- **v7 (this document):** All v6 reservations addressed. AgentKind extended to 7 values to cover helper/subagent profiles. Plan is APPROVED.
