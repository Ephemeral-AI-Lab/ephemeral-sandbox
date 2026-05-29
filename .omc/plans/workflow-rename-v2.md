# Plan v2: Rename TaskCenter durable UNIT "Goal" → "Workflow" (semantic, occurrence-by-occurrence)

> **Supersedes** `.omc/plans/workflow-vocabulary-rename.md` (v1). v1's consumer
> maps, phase structure, and snake_case/PascalCase grep-gate technique are reused
> verbatim where still correct. v1's KEEP/RENAME *decisions* are REPLACED: v1
> kept serialized entity tokens as `goal` for stability; the locked target model
> below requires entity-axis tokens to BECOME `workflow` **even when serialized**
> — those are migrated in a mandatory second gated commit (Phase D), NOT silently
> kept.
>
> **This revision (consensus ITERATE) bakes in the following LOCKED user
> decisions; there are NO remaining open decisions:**
> - **D1 — DB/wire migration is a MANDATORY second gated commit in the same
>   effort.** The in-process symbol+package rename is commit 1 (Phase A); the DB +
>   persisted-value migration is commit 2 (Phase D). Phase D is REQUIRED — not
>   optional, not deferrable. All "defer indefinitely" / "deferrable" language is
>   removed.
> - **D2 — planner-facing tokens read as the OBJECTIVE, so they KEEP `goal`
>   entirely** (no rename, no Phase D): tool names `submit_plan_closes_goal` /
>   `submit_plan_defers_goal`; EventType members `PLANNER_COMPLETES_GOAL_PLAN` /
>   `PLANNER_DEFERS_GOAL_PLAN` (member AND value); scenario class
>   `PlannerDefersWithoutDeferredGoal`; context recipe `context.goal_entry_minimal`;
>   and the `_inspect_prompt` check key `closes_goal_terminal`. The check key stays
>   `goal` because the tool it checks (`submit_plan_closes_goal`) stays `goal` —
>   resolving the closes_goal_terminal/tool-name coherence the Critic flagged.
> - **Defaults adopted as locked:** `goal_id → workflow_id` lands atomically in the
>   Phase D commit (DB column + FK + `uq_iteration_goal_sequence` + `ScopeField` +
>   `ContextPacket` + `AuditNode`/`NodeId` + `task_center_goal_id` + on-disk jsonl +
>   mock reads + tests). `ContextEngineDeps.goal_store → workflow_store` renames in
>   Phase A. File renames `db/models/goal.py → workflow.py` and
>   `db/stores/goal_store.py → workflow_store.py` happen in Phase A.
>
> **Citation caveat (two corrected paths, verified):**
> - `submit_execution_handoff.py` real path is
>   `backend/src/tools/submission/executor/submit_execution_handoff/submit_execution_handoff.py`
>   (the plan's short-form `submit_execution_handoff.py` refers to this file).
> - the planner tool-name constants live in `backend/src/tools/_names.py` (top-level);
>   `tools/submission/planner/_names.py` does NOT exist (verified — `planner/` has only
>   `__init__.py`, `_schemas.py`, and the two tool dirs).
>
> The remaining per-token line numbers are best-effort and may drift under
> parallel-agent edits. The executor should **re-derive every edit location at edit
> time via LSP + grep gates** and trust those over this plan's line numbers.

## 1. Locked target model + the one-line discriminator

**Target model (user-locked):** the durable top-level UNIT is renamed
**`Goal` → `Workflow`**. The model is now **Workflow → Iteration → Attempt**.
**Crucial new semantic:** *each Workflow HAS its own goal* — the word "goal" now
denotes the OBJECTIVE / work-statement the workflow pursues, not the unit.

A blind find-replace `goal → workflow` is **explicitly WRONG and rejected.** Every
occurrence is classified by REGISTER, then decided mechanically:

| register | what the token denotes | decision | phase |
|---|---|---|---|
| **objective_content** | the OBJECTIVE / "what to accomplish" text the unit pursues (`Goal.goal:str`, `<goal>`/`<iteration_goal>` XML, `deferred_goal_for_next_iteration`, `goal_handoff` arg, `iteration_goal` param) | **KEEP** (stays `goal`) | none |
| **planner-facing objective token** (D2) | tool names `submit_plan_closes_goal`/`submit_plan_defers_goal`, `PLANNER_*_GOAL_PLAN` members+values, `PlannerDefersWithoutDeferredGoal`, `context.goal_entry_minimal`, `closes_goal_terminal` check key | **KEEP** (reads as the objective the plan pursues) | none |
| **entity_axis**, rename does NOT change a persisted/on-disk/cross-process byte | the durable UNIT / its identity / lifecycle / store, as in-process Python symbols, in-repo-only string-matches, in-repo-only check keys, ENTITY EventType **member names**, in-memory `RunReport` dict keys, in-repo closure-report asserts | **RENAME** → `workflow` form | **Phase A** |
| **entity_axis**, rename CHANGES a persisted/on-disk/cross-process byte | DB DDL (`goals` table, `goal_id` column/FK/constraint), persisted status/audit VALUE strings (`waiting_goal`, `goal_start`, `goal_closure_report` payload value, `task_center_goal_id`, `goal_id` jsonl key), on-disk artifacts (`goal.json`, `sandbox_events.jsonl`, capacity/full-stack on-disk summary JSON) | **RENAME** → `workflow` form | **Phase D** (mandatory 2nd gated commit) |

**The discriminator in one line:** *Is the token the work-statement TEXT
(→ KEEP `goal`) or the UNIT's identity/lifecycle/store (→ `workflow`)?* And for the
phase split: **a token is Phase-D ONLY if renaming it changes a
persisted/on-disk/cross-process byte (a DB column/table, a serialized VALUE string,
or an on-disk artifact). Otherwise it goes in Phase A** — including in-repo-only
string-matches, ENTITY EventType *members* (both NAME and value — verified no
persisted/string-match consumer, see §3(e)), in-memory `RunReport` keys, and in-repo
closure-report asserts, because the Phase A pytest sweep validates them and one miss
fails an assert before the commit. The
disambiguator for the overloaded bare string: a trailing `_id` / store / status /
table / lifecycle handle ⇒ entity; a bare `goal` field/tag/arg carrying text ⇒
objective.

---

## 2. Merged, deduped DECISIONS TABLE

Columns: token | kind | register | decision | proposed | serialized | consumers (abbrev.) | rationale. Paths are under `backend/src` / `backend/tests` unless noted.

### 2A. RENAME — pure in-process entity-axis symbols (no DB/wire/string-match)

| token | kind | proposed | consumers | rationale |
|---|---|---|---|---|
| `GoalClosureDeliveryResult` / `GoalClosureDeliveryStatus` | dto | `WorkflowClosureDeliveryResult` / `WorkflowClosureDeliveryStatus` | `task_center/goal/state.py:104,108`; `closure_report_router.py:16,30,48,73,79,85,95` | Internal delivery DTOs; not in facade `_EXPORTS`, not persisted, not string-matched. |
| `GoalClosureCallback` | type alias | `WorkflowClosureCallback` | `goal/lifecycle.py:49,63,253` | Local `__all__` alias. |
| `GoalLifecycle` | class | `WorkflowLifecycle` | `goal/lifecycle.py:52,255`; `goal/starter.py:18,146,151` | Lifecycle machinery; NOT in facade `_EXPORTS` (verified). |
| `create_goal` / `close_goal` / `_require_goal` | method | `create_workflow` / `close_workflow` / `_require_workflow` | `goal/lifecycle.py:76,125,92,...`; `goal/starter.py:76` | In-process methods. **KEEP** the `goal=` kwarg of `create_goal` (objective text) and `goal_id` params (entity FK → see FLAG). |
| `nested_goal_depth` + `_nested_goal_depth_gt_1` | function | `nested_workflow_depth` / `_nested_workflow_depth_gt_1` | `goal/ancestry.py:19,60`; `_core/terminal_tool_routing.py:22,42,51`; **test monkeypatch** `test_agent_launch/test_terminal_tool_router.py` (5 sites, full module path) + `test_domain/test_ancestry.py` (import) | Counts UNIT nesting. NOTE: `_nested_goal_depth_gt_1` is a documented monkeypatch target string — its 5 patch strings + the import must move in the SAME commit. (Borderline serialized; treated as RENAME because the only "serialization" is in-repo test strings updated in lockstep, no wire/DB.) |
| `child_outcomes_for_goal` | function | `child_outcomes_for_workflow` | `attempt/orchestrator.py:21,226`; `_core/generator_summaries.py:211-216,295` | Symbol imported directly (not string-matched). Couples to `list_for_goal` (see FLAG). |
| `parent_task_for_delegated_goal` | method | `parent_task_for_delegated_workflow` | `attempt/deps.py:105`; `goal/starter.py:192,280`; `goal/closure_report_router.py:58` | In-process handle method. |
| `mark_waiting_goal` / `restore_running_after_failed_goal_start` | method | `mark_waiting_workflow` / `restore_running_after_failed_workflow_start` | `attempt/deps.py:138,169`; `goal/starter.py:200,284` | In-process methods. KEEP the `goal=` kwarg of `mark_waiting_goal` (objective text → summary payload key `goal`). |
| `AttemptDelegatedGoalParentTask` | class | `AttemptDelegatedWorkflowParentTask` | `attempt/deps.py:120,107,111` | Pure-internal axis dataclass; not exported/persisted/string-matched. |
| `_PreparedGoalOrigin` | class | `_PreparedWorkflowOrigin` | `goal/starter.py:120,124,141,312` | Pure-internal axis dataclass. |
| `assert_goal_open` | function | `assert_workflow_open` | `_core/invariants.py:25,...,148`; lifecycle/iteration callers + tests | Lifecycle invariant over the UNIT; exported in `__all__` but in-repo only. |
| `GoalLifecycle`-internal `_build_goal_lifecycle` | method | `_build_workflow_lifecycle` | `goal/starter.py:146,151` | Internal builder. |
| `run_id_for_attempt` local `goal` var + `"Goal ... not found"` msg | attribute/prose | `workflow` | `attempt/deps.py:90-95` | Local var binds the UNIT DTO; prose message. |
| `start_delegated_goal` | method | `start_delegated_workflow` | `tools/submission/context/executor.py:71,25`; `submit_execution_handoff.py:82`; `test_tools/*` stubs | Starts a delegated UNIT. KEEP `goal_handoff` kwarg (objective). |
| `is_entry_origin_goal` / `is_recursive_goal` + module `goal_origin.py` | function/module | `is_entry_origin_workflow` / `is_recursive_workflow` / `workflow_origin.py` | `task_center_runner/scenarios/_scenario_helpers/goal_origin.py`; re-exports + ~6 scenario callers | Predicates over the UNIT origin; in-scope helper symbols. Confirm not test-string-matched before renaming. |
| `ScenarioContext.goal` field (holds the `GoalRecord` entity) | field | `ScenarioContext.workflow` | `scenarios/base.py:32`; `scenario_adapter.py:80,86`; `runner.py:896,902`; `_scenario_helpers/goal_origin.py`; `full_case_user_input.py:390`; `full_stack_adversarial.py:458` | In-memory dataclass holding the entity. KEEP the **inner** `.goal` (= `ctx.workflow.goal`, objective text). |
| `make_goal_request_after_edit_reminder` + module `request_goal_after_edit.py` | function/module | `make_workflow_request_after_edit_reminder` / `request_workflow_after_edit.py` | `tools/submission/notification_triggers/request_goal_after_edit.py:24`; `__init__.py:6-7` | Factory symbol + file. The RULE-NAME string is FLAG (see 2C). |
| graph_summary loop var `goal` (per-entry `goal.id/.status/...`) | attribute | `workflow` | `task_center_runner/core/runner.py:100,102,139-143` | Local loop var over `GoalRecord`; the dict KEY `"goals"` is FLAG (see 2C). |
| Test-local helpers `_seed_goal`/`_build_goal`/`make_goal`/`_seed_nested_goal`, taxonomy constants `PLANNER_COMPLETES_GOAL`/`RECURSIVE_GOAL`/`_FakeGoal`/local `goal` vars | function/attribute | `*_workflow` forms | module-local in `backend/tests/**` + `task_center_runner/tests/**` | Module-local test symbols, not string-matched cross-module. **Verify each is module-local** (no cross-module string assertion of its VALUE) before renaming; if a VALUE is asserted, reclassify as FLAG. |
| **`.py` entity-unit DOCSTRING/COMMENT prose** naming the UNIT (`_core/primitives.py:41,44`; `entry/bootstrap.py:4,5,74,91`; `_core/invariants.py:4`; `generator_summaries.py:214`; `terminal_tool_routing.py:34,36,52,57`; moved `workflow/*.py` (was `goal/*.py`) docstrings; `db/models/workflow.py` (was `goal.py`) + `db/stores/workflow_store.py` (was `goal_store.py`) docstrings; scenario docstrings) | prose | `workflow` (per-sentence: keep `goal` where it means the objective) | as listed | **MOVED INTO PHASE A** (Blocking #1): the bare-`Goal` English-word prose in these `.py` files would make the Phase A `\b(Goal\|...)\b → 0` gate unsatisfiable if left to a later phase. Editing it in Phase A makes the gate satisfiable at Phase A close. **Per-sentence judgment** — the same sentence may say "a Goal[unit] is the objective[its goal]"; the unit word renames, the objective word stays. (Only `docs/architecture/**.html` + `CLAUDE.md` prose — outside the gate's `backend/src backend/tests` scope — stay in the later prose phase, Phase C.) |

### 2B. KEEP — objective_content (the workflow's goal; stays `goal`)

| token | kind | serialized | consumers (abbrev.) | rationale |
|---|---|---|---|---|
| `Goal.goal: str` field / `goal=` kwarg | field/arg | yes | `goal/state.py:57`; db `goal.py:33` (`goal` column); `goal_store.py:28,42,82,125`; `_core/persistence.py:52,82`; tests `goal="goal"` | The OBJECTIVE the workflow pursues — *each Workflow HAS its own goal*. KEEP overrides serialized. New model: `Workflow.goal` (column unchanged). |
| `goal` DB column (models/goal.py + models/iteration.py) | db_column | yes | `goal.py:33`; `iteration.py:30`; stores + `audit/recorder.py:108,124` `"goal"` key | Per-row objective text. KEEP despite persisted + audit key. |
| `Iteration.goal` field + `iteration_goal` param | field/param | yes | `iteration/state.py:33`; `goal/lifecycle.py:99,119,164,170,176`; `_core/persistence.py:82` | Per-iteration objective text. |
| `deferred_goal_for_next_iteration` (field/param/arg/tag) + `deferred_goal` DB column | field/arg/xml | yes | `iteration/state.py:37`; `attempt/state.py:44`; `submissions.py:35`; planner tools; `iteration_store.py`/`attempt_store.py` (`deferred_goal` column); `tag_dictionary.py:66`; many tests | The objective deferred to the next iteration. Explicit objective example. KEEP. |
| `SuccessDeferred.deferred_goal_for_next_iteration` | field | no | `iteration/state.py:83,84`; `attempt_coordinator.py:260,268,269` | Deferred objective text. |
| `DEFERRED_GOAL_CONTINUATION` enum member (`= "partial_continuation"`) | enum_value | yes | `iteration/state.py:21`; `goal/lifecycle.py:118` | References the deferred objective; value has no `goal`. |
| `assert_predecessor_has_deferred_goal_for_next_iteration` | function | yes | `_core/invariants.py:45,144` + callers | Asserts presence of the deferred OBJECTIVE; tracks the kept field name. |
| `<goal>` XML tag + `source_kind="goal"` + `metadata={"tag":"goal"}` + `GOAL_STATEMENT`/`"goal_statement"` block kind | xml_tag/enum_value | yes | `context_engine/renderer.py:39`; `recipes/iterations.py:78,82,83`; `tag_dictionary.py:38`; `packet.py:33`; `_task_xml.py:40`; many `test_context_engine/*` + mock `runner.py:1768,1781` `"goal" / "<goal>"` checks | Renders the OBJECTIVE to the model. KEEP. A blind rename here would break the renderer `_DEFAULT_TAGS` map AND the mock `_inspect_prompt`. |
| `<iteration_goal>` tag + `ITERATION_STATEMENT`/`"iteration_statement"` block kind + `child_tag iteration_goal` | xml_tag/enum_value | yes | `renderer.py:40`; `packet.py:34`; `recipes/iterations.py:107`; `tag_dictionary.py:55`; `role_directives.py:18`; mock `test_initial_messages_capture.py:162` | Per-iteration objective tag. |
| `<deferred_goal_for_next_iteration>` tag + `goal_iteration_blocks()` + `_goal_statement_block`/`_current_iteration_goal_child` + `(identical to <goal>)` marker | xml_tag/function/value | mixed | `tag_dictionary.py:66`; `recipes/iterations.py:53,56,69,...`; `recipes/planner.py:24,48`; `test_iteration_no_invariant.py:80` | All emit/label objective text. KEEP. |
| `goal_handoff` arg + `_validate_goal_handoff` | arg/method | yes | `submit_execution_handoff.py:40,50-55,71,82`; `tools/submission/context/executor.py:72,78`; mock `runner.py:413,435` | The objective being handed off (→ `prompt=`). KEEP (wire arg name; keeping required). |
| `recursive_handoff_goal(ctx)` method + its returned objective text + `_CONTINUATION_GOAL`/`_CHILD_GOAL` consts | method/value | no | `scenarios/base.py:61,88`; mock `runner.py:408,430`; pipeline scenarios | Returns objective text handed to a child. KEEP (method name describes the objective). |
| `submit_plan_closes_goal` / `submit_plan_defers_goal` tool NAMES (+ `SUBMIT_PLAN_*_GOAL_TOOL_NAME` constants, `SubmitPlanClosesGoalInput`/`SubmitPlanDefersGoalInput` DTOs, dir names) — **D2: KEEP (LOCKED)** | function/import/dto | yes | `tools/submission/planner/*` (dirs `submit_plan_closes_goal`/`submit_plan_defers_goal` + `_factory.py` + `_schemas.py`) + ~30 scenarios + mock matches + `_names.py` + registry + intent-drift test | "closes/defers the GOAL the plan pursues" — reads as the objective ("plan FOR the goal"). **KEEP per D2.** No rename, no Phase D. Persisted VALUES already carry no entity `goal` (`planner_full_plan`); renaming would span ~30 scenarios + the mock `.name` map + intent-drift FQ-path + agent frontmatter for zero conceptual gain; pairs with the kept `deferred_goal_for_next_iteration` arg inside the same tool. |
| `PLANNER_COMPLETES_GOAL_PLAN` / `PLANNER_DEFERS_GOAL_PLAN` EventType members (+ values `planner_full_plan`/`planner_partial_plan`) — **D2: KEEP (LOCKED)** | enum_value | yes | `task_center_runner/audit/events.py:62,63`; mock `runner.py:110,111` (`.name` map); ~20 scenarios | Planner *plan* events (the plan FOR the objective), not the workflow's own lifecycle. **KEEP member AND value per D2.** Verified: values are `planner_full_plan`/`planner_partial_plan` (no `goal` token). |
| `PlannerDefersWithoutDeferredGoal` scenario class + registry `planner_validation.defers_without_deferred_goal` — **D2: KEEP (LOCKED)** | class | yes | `defers_without_deferred_goal.py:24,27,34`; `planner_validation/__init__.py`; `scenarios/__init__.py`; `pack_catalog.py:262,266`; tests `test_scenario_suite_imports.py:143`, `test_focused_scenarios.py:187`, `test_capacity_scenario_packs.py:105` | The "deferred goal" is the continuation OBJECTIVE the planner failed to supply. **KEEP per D2.** |
| `context.goal_entry_minimal` capacity-pack name — **D2: KEEP (LOCKED)** | metadata_value | yes | `pack_catalog.py:238` | Names the context recipe that renders the objective-bearing entry. **KEEP per D2.** |
| `closes_goal_terminal` `_inspect_prompt` check KEY — **D2: KEEP (LOCKED)** | metadata_key | yes (in-repo string) | mock `scenario_loop_runner.py:218`/`runner.py:1772`; test `test_runner_imports.py:187` | Tracks the decision of the `submit_plan_closes_goal` tool, which stays `goal` per D2 — so the key stays `goal` for coherence (resolves the closes_goal_terminal/tool-name incoherence the Critic flagged). The sibling key `"goal"` (detects `<goal>`) also stays. **KEEP per D2.** |
| `EventType.ITERATION_FROM_DEFERRED_GOAL_CREATED` | enum_value | yes | `audit/events.py:55` (value `iteration_continuation_created`) | Names creation from a deferred GOAL (objective). Value has no `goal`. |
| Generic English: `Goal-Driven Execution`, "verifiable goals" (`.claude/CLAUDE.md:45,49`); "Non-goals and Myths" (`docs/architecture/sandbox/space-model.html`) | prose | no/yes | as listed | Not the durable unit. Unrelated to the rename. |
| Test data string values `goal="goal"` / `text="goal"` / `request_prompt="goal"` | fixture/prose | no | `test_recipes_planner_closes_or_defers.py:42`; `test_renderer.py:88`; `test_sweevo_audit_recorder.py:99` | Placeholder objective text. |

### 2C. RENAME — entity_axis serialized/contract tokens (split Phase A vs Phase D by the byte-change test)

Per the re-partition (§6 Option 2C, chosen): a row is **Phase D** only if renaming
it changes a persisted/on-disk/cross-process byte; everything else is **Phase A**
(validated by the Phase A pytest sweep — one miss fails an assert before commit).
Each row below carries its phase. Grouped by the migration/cluster they belong to.

**DB / ORM layer — Phase D (the DDL migration; see §3(a) for the concrete mechanism):**

| token | phase | kind | proposed | consumers (abbrev.) |
|---|---|---|---|---|
| `goals` DB table (`__tablename__`) | **D** | db_table | `workflows` | `db/models/goal.py:21` (becomes `workflow.py`); FK `db/models/iteration.py:25` `ForeignKey("goals.id")`; `engine.py` doc; `drop_legacy_tier_tables.py` doc; **test** `test_migration_drops_legacy_table.py:42` `assert "goals" in tables` |
| `ForeignKey("goals.id")` | **D** | db_column | `ForeignKey("workflows.id")` | `db/models/iteration.py:25` |
| `goal_id` DB column on `iterations` | **D** | db_column | `workflow_id` | `db/models/iteration.py:23,56`; `iteration_store.py:23,33,96-119,158`; `iteration/state.py:30` (`Iteration.goal_id`); `attempt/launch.py:318-397`; `goal/ancestry.py`; `_core/persistence.py:55-111`; `_core/terminal_tool_routing.py:39-43,134`; `_core/generator_summaries.py:212,216`; `audit/recorder.py:121,562-654`; mock `runner.py:896,422,444,1971`; `scenario_adapter.py:80`; tests (many) |
| `UniqueConstraint name="uq_iteration_goal_sequence"` | **D** | db_column | `uq_iteration_workflow_sequence` | `db/models/iteration.py:54-59` |
| `GoalRecord` (ORM class) | **A** | class | `WorkflowRecord` | `db/models/goal.py:18`; `db/models/__init__.py:4,17`; `goal_store.py:9,...`; `audit/recorder.py:35,102,295,300,552` (pure in-process Python class; LSP-renamed — KEEP `__tablename__="goals"` and `goal`/`requested_by_task_id` columns) |
| `GoalStore` (class) + module `db.stores.goal_store` path + file `goal_store.py→workflow_store.py` | **A** | class/module | `WorkflowStore` / `db.stores.workflow_store` | `goal_store.py:19`; `db/stores/__init__.py:10,20-22` (`_EXPORTS` string keys + module-path string); broad importers + test conftests. File rename + module-path strings move with the `git mv` (default locked). |
| `goal/` package dir + module paths (`task_center.goal.*`) | **A** | package | `task_center/workflow/` | `task_center/__init__.py:59-60,102-113,131` (`_EXPORTS` module-path strings); every `from task_center.goal.* import`; tests |

(NOTE: the *symbol/class/module/package* renames are Phase A — renaming a Python
class or import path changes no persisted byte. Only the `goals` **table name**, the
`goal_id` **column/FK/constraint**, and the `goal`/`deferred_goal` text columns
(KEEP) live in the DB. The latter three are the only DB rows that touch a persisted
byte, hence Phase D.)

**Facade `_EXPORTS` (string-keyed public contract — `task_center/__init__.py`) — Phase A (in-repo Python only; LSP-renamed):**

| token | kind | proposed | consumers |
|---|---|---|---|
| `Goal` (class) | dto | `Workflow` | `goal/state.py:52`; `_EXPORTS["Goal"]` + module path `task_center.goal.state`; persistence/invariants/recipes/db/tests |
| `GoalStatus` (enum) | enum | `WorkflowStatus` | `goal/state.py:44`; `_EXPORTS`; lifecycle/starter/persistence/db/tests. **Enum VALUES `open/succeeded/failed/cancelled` KEEP** (no `goal` token; needless migration). |
| `GoalOrigin` (dto) | dto | `WorkflowOrigin` | `goal/state.py:17`; `_EXPORTS`; `entry/bootstrap.py`; `tools/submission/context/executor.py:74,79`; db. **`.entry`/`.task` value strings KEEP.** |
| `GoalOriginKind` (enum) | enum | `WorkflowOriginKind` | `goal/state.py:11`; `_EXPORTS`; `closure_report_router.py:19,31`; `db/models/goal.py:29` column. **`entry`/`task` values KEEP.** |
| `GoalStarter` (class) | class | `WorkflowStarter` | `goal/starter.py:51`; `_EXPORTS`; `entry/bootstrap.py`; `tools/submission/context/executor.py`; reflection test `test_saga_inline_equivalence.py` (`getsource` of `GoalStarter._compensate_failed_start`) |
| `StartedGoal` (dto) + `.goal_id` (entity id; `.goal` field KEEP) | dto/field | `StartedWorkflow` / `.workflow_id` | `goal/starter.py:38-44,69,111`; `_EXPORTS`; `tools/submission/context/executor.py:17,73`; `submit_execution_handoff.py:36,81,90,97` (`.goal_id` → audit `goal_id`) |

**Closure-report cluster (Phase A symbol renames; Phase D for the persisted VALUE + `goal_id` payload):**

| token | phase | kind | proposed | consumers |
|---|---|---|---|---|
| `GoalClosureReport` (dto) | **A** | dto | `WorkflowClosureReport` | `goal/state.py:81`; lifecycle/starter/router; `attempt/orchestrator.py:43,166,215`; `orchestrator_registry.py:17,33`; `attempt/deps.py:35,128`; tests |
| `GoalClosureReportRouter` (class) | **A** | class | `WorkflowClosureReportRouter` | `goal/closure_report_router.py`; tests `test_phase04_close_report_delivery.py` |
| `apply_goal_closure_report` (method + Protocol) | **A** | method | `apply_workflow_closure_report` | `attempt/orchestrator.py:166`; `orchestrator_registry.py:33`; `attempt/deps.py:128,136`; `goal/closure_report_router.py:72`; tests (4 sites). Method renames freely (no persisted byte); the payload **VALUE string** `"goal_closure_report"` is the next row. |
| `"goal_closure_report"` payload KEY + `submission_kind="goal_closure_report"` VALUE | **D** | metadata_key/value | `"workflow_closure_report"` | **producer** `attempt/orchestrator.py:204,205`; **consumers** mock `runner.py:1968`; tests `test_submission_terminal_routing.py:350`, `test_attempt_orchestrator.py:495`. This is a cross-process submission-payload VALUE byte → **Phase D** (bundle with the other persisted VALUES). |
| `GoalClosureReport.goal_id` (+ asdict payload key `goal_id`) | **D** | metadata_key | `workflow_id` | `attempt/orchestrator.py:204,226`; mock `runner.py:1971-1974`. Rides the `goal_id→workflow_id` atomic rename → Phase D. |

**ENTITY-lifecycle EventType members — member NAME *and* value both in Phase A (edited whole):**

Both the member NAME and the value move in Phase A because these entity-lifecycle
event VALUES are **NOT persisted to jsonl and have no string-match consumer** —
verified: `recorder.py:616-617` only writes events whose `type.value`
starts with `sandbox_` (or `type.name` with `SANDBOX_`). **Verified:** grepping the
value literals (`"goal_started"`, `"goal_completed"`, `"goal_requested"`,
`"recursive_goal_requested"`, `"recursive_goal_completed"`) finds them ONLY at the
enum definition RHS — no string-match, no persisted-jsonl, no on-disk consumer.
Therefore, by the plan's byte-change rule, BOTH the member NAME and its VALUE string
move in Phase A: edit each `WORKFLOW_* = "workflow_*"` line WHOLE in the symbol sweep.
This eliminates the transient `WORKFLOW_STARTED = "goal_started"` member/value
incoherence — there is no reason to split these across phases. (Contrast: the
genuinely-persisted VALUE strings `waiting_goal`/`goal_start`/`goal_closure_report`
payload value/`task_center_goal_id`/`goal_id` jsonl key DO move in Phase D — see the
next block.)

| token | phase | kind | proposed | consumers |
|---|---|---|---|---|
| EventType `GOAL_STARTED`/`GOAL_COMPLETED`/`GOAL_REQUESTED` (member NAME + value, edited whole) | **A** | enum_member+value | `WORKFLOW_STARTED = "workflow_started"` / `WORKFLOW_COMPLETED = "workflow_completed"` / `WORKFLOW_REQUESTED = "workflow_requested"` | `task_center_runner/audit/events.py:50-52` (whole line); in-process `.name`/member references. Value has NO persisted/string-match consumer (verified) → Phase A. |
| EventType `RECURSIVE_GOAL_REQUESTED`/`RECURSIVE_GOAL_COMPLETED` (member NAME + value, edited whole) | **A** | enum_member+value | `RECURSIVE_WORKFLOW_REQUESTED = "recursive_workflow_requested"` / `RECURSIVE_WORKFLOW_COMPLETED = "recursive_workflow_completed"` | `audit/events.py:74,75` (whole line); mock `runner.py:418,440,842` (member refs); scenarios `full_stack_adversarial.py:62,63`, `nested_goal.py:91,92,141`, `deferred_parent_planner_terminal_routing.py:97,103` (member refs). Value has NO persisted/string-match consumer (verified) → Phase A. |

**Persisted task-status + submission/audit metadata VALUES — Phase D (each is a cross-process/on-disk byte):**

| token | phase | kind | proposed | consumers |
|---|---|---|---|---|
| `WAITING_GOAL` enum member (**NAME → A**) + value `"waiting_goal"` (**VALUE → D**) | A/D | enum_member/value | `WAITING_WORKFLOW` / `"waiting_workflow"` | `_core/task_state.py:30`; `attempt/deps.py:160,172`; `attempt/orchestrator.py:182,198`; `goal/starter.py:288,289`; `goal/closure_report_router.py:53`; `_core/generator_summaries.py:37,91`; tests (`test_submission_terminal_routing.py:215,273`, `test_phase04_*`, `test_attempt_orchestrator.py:449,476,522`, `test_generator_dag.py:84`). **The VALUE `"waiting_goal"` is a persisted task-status string compared via `set_task_status_if_current` → Phase D; the member NAME `WAITING_GOAL` renames in Phase A.** |
| `submission_kind="goal_start"` + outcome `"goal_start"` | **D** | metadata_value | `"workflow_start"` | `submit_execution_handoff.py:94`; `attempt/deps.py:147`; downstream audit consumers |
| `delegated_goal_id` arg (**A**) → payload key `goal_id` (**D**) | A/D | arg/metadata_key | `delegated_workflow_id` arg; payload key `workflow_id` | `attempt/deps.py:141,150`; `goal/starter.py:204`. Arg NAME renames in Phase A; the emitted payload KEY rides the `goal_id→workflow_id` Phase D rename. |
| `request_recursive_goal:` action string (startswith match) | **D** | metadata_value | `request_recursive_workflow:` | mock `runner.py:404`; scenarios `full_case_user_input.py`, `nested_goal.py`, `deferred_parent_planner_terminal_routing.py`. **Cross-process action-string byte → Phase D** (producer + every startswith match in lockstep). |
| `recursive_goals` close-report metric key (on-disk summary JSON) | **D** | metadata_key | `recursive_workflows` | `full_stack_tool_scripts.py:847,863` (serialized to disk via `write_file` tool, `json.dumps` line 1169); test `test_full_stack_adversarial.py:393`. **On-disk artifact byte → Phase D.** |

**Cross-system metadata correlation id (widest blast radius) — Phase D (rides the `goal_id→workflow_id` atomic rename across DB + wire + jsonl; default LOCKED):**

| token | phase | kind | proposed | consumers |
|---|---|---|---|---|
| `task_center_goal_id` (metadata key + `SandboxCaller`/`ExecutionMetadata` field) | **D** | metadata_key/field | `task_center_workflow_id` | producer `attempt/launch.py:114,115`; `tools/_framework/core/runtime.py:40,89` (`_TYPED_FIELDS`); `tools/sandbox/_lib/tool_context.py:26`; `sandbox/shared/models.py:34` + `audit_fields()`; `sandbox/audit/translation.py:138`; `sandbox/ephemeral_workspace/plugin/op_context.py:30` (`_CALLER_AUDIT_FIELDS`); `engine/audit/stream.py:86`; mock `probes.py:79`, `runner.py:1900`; tests `test_plugin_host_dispatch.py:109,146`, `test_contract.py:148`, `test_operation.py:74`, `test_submission_helper_tools.py:46`. Persisted to `sandbox_events.jsonl` → Phase D. |
| `AgentLaunch.goal_id` field + `launch.goal_id` | **D** | field | `workflow_id` | `attempt/deps.py:64`; `attempt/launch.py:114-397`. Rides the atomic `goal_id→workflow_id` rename (the field feeds the persisted correlation id). |
| `AuditNode.goal_id` kwarg (+ field def) + `NodeId.goal_id`/`goal_seq` | **D** | arg/field | `workflow_id` / `workflow_seq` | def `audit/base.py:24` (shared module, no listed owner); `engine/audit/stream.py:86`; `sandbox/audit/translation.py:138`; `task_center_runner/audit/legacy.py:67`, `node_id.py:25,26`; persisted `sandbox_events.jsonl` → Phase D. |
| `goal_id` as `ScopeField` Literal + `ContextRefs.goal_id` + `require_field("goal_id")` + recipe `canonical_refs goal_id=` | **D** | field | `workflow_id` | `context_engine/scope.py:22,29,60-98`; `packet.py:46` (persisted into `ContextPacket`); `recipes/planner.py:33,37,66`, `generator.py:43,49,92`, `evaluator.py:43`; `_core/terminal_tool_routing.py:39,43`. The `ScopeField` Literal + `require_field` string + persisted `ContextPacket` ref are cross-process bytes → Phase D. |

**Store-protocol method names (Protocol contract; in-repo Python, cross-package) — Phase A for the symbol/attribute, Phase D for the `goal_id` params (atomic):**

| token | phase | kind | proposed | consumers |
|---|---|---|---|---|
| `GoalStoreProtocol` + `goal_store` attribute (DI field) | **A** | class/attribute | `WorkflowStoreProtocol` / `workflow_store` | `_core/persistence.py:41,208`; `attempt/deps.py:23,70,90`; `context_engine/core.py:32,59`; `goal/{lifecycle,ancestry,starter,closure_report_router}.py`; `entry/bootstrap.py`; `db/stores/goal_store.py`; mock `runner.py:896`, `scenario_adapter.py:80`; `task_center_runner/core/stores.py:25-103`, `engine.py:243`; **31 src + ~432 test** occurrences of the `goal_store` fixture/attr name. Pure in-process DI symbol — Phase A (default LOCKED); gated by the §5 `goal_store → 0` grep. |
| `list_for_goal` (IterationStoreProtocol) method NAME (**A**) + `goal_id` params on `append_iteration_id`/`set_status`/`get`/`get_by_sequence` (**D**) | A/D | method/param | `list_for_workflow` / `workflow_id` | `_core/persistence.py:55-111`; `recipes/planner.py:51`; `generator_summaries.py:216`; `iteration_store.py:97`; `goal_store.py`. Method NAME `list_for_goal` is in-repo Python → Phase A; the `goal_id` PARAMS ride the column rename → Phase D. |
| invariants `assert_iteration_id_unique_in_goal` / `assert_iteration_sequence_contiguous` (goal-keyed) | **A** | function | `assert_iteration_id_unique_in_workflow` | `_core/invariants.py:30,37,...`; lifecycle/iteration callers + tests. In-repo Python symbol → Phase A. |

**Run-report / audit-artifact layout — split by the byte-change test (verified):**

There are TWO distinct `"goals"` keys; they split between phases:

| token | phase | kind | proposed | consumers |
|---|---|---|---|---|
| `graph_summary["goals"]` dict key (the **in-memory `RunReport.graph_summary`** field) | **A** | metadata_key | `"workflows"` | producer `core/runner.py:99,137,147` (`_graph_summary` returns `{"goals": ...}`); consumed ONLY by in-repo test asserts (`test_correctness.py:71`, `_focused_scenario_contracts.py:62,104`, `test_full_case_user_input.py`, `test_initial_messages_capture.py:79`, `test_correctness_via_event_source.py:83`, `test_scenario_loop_runner_planner_submit.py:106`, sandbox tests, ~14 files). **VERIFIED not written to disk** (no `atomic_write_json`/`append_jsonl` consumer; `RunReport` is in-memory, `base.py:40`) → **Phase A** (producer + every test assert in the same pytest sweep). |
| capacity on-disk summary `graph["goals"]` key + `recursive_goals` (in `capacity_actions/metrics.py:58` + `full_stack_tool_scripts.py:847,863`) | **D** | metadata_key | `"workflows"` / `recursive_workflows` | These are serialized to disk via the `write_file` tool (`metrics.py` → `.ephemeralos/sweevo-mock/capacity/full-system-capacity-summary.json`, schema `live_e2e.capacity.v1`; `full_stack_tool_scripts.py` `json.dumps` line 1169). **On-disk artifact bytes → Phase D.** (Distinct from the in-memory `graph_summary` key above.) |
| `goal.json` filename + `goal_<NN>_<id>` audit DIR name (on-disk) | **D** | value | `workflow.json` / `workflow_<NN>_<id>` | `audit/recorder.py:559,641`; test globs `test_correctness.py:85-89`, `test_full_case_user_input.py:202-209`, `test_sweevo_audit_recorder.py:154-186`. The written filename + dir name are on-disk bytes → **Phase D.** |
| `_serialize_goal`/`_handle_goal`/`_ensure_goal_dir`/`_goal_dir`/`_goal_seq_counter` (recorder internal symbols) | **A** | function/attr | `_serialize_workflow`/`_handle_workflow`/`_ensure_workflow_dir`/... | `audit/recorder.py:102,212,219,297,302,552-654`. Pure in-process recorder methods/attrs (LSP-renamed); they emit the on-disk names above but renaming the SYMBOL changes no byte → **Phase A.** (KEEP the `"goal"` payload KEY inside `_serialize_goal` — it is `record.goal` objective text, line 108.) |

**Tool / trigger / scenario / hook NAMES (in-repo registry + frontmatter + test string-match) — Phase A (in-repo-only, no DB/on-disk byte):**

> The D2 KEEPs (`submit_plan_closes_goal`/`submit_plan_defers_goal`,
> `PlannerDefersWithoutDeferredGoal`, `context.goal_entry_minimal`,
> `closes_goal_terminal`) are NOT in this table — they stay `goal`, listed under 2B.

| token | phase | kind | proposed | consumers |
|---|---|---|---|---|
| `request_goal_after_edit` notification-rule NAME + factory dict key | **A** | metadata_value | `request_workflow_after_edit` | `request_goal_after_edit.py:38`; `notification_triggers/__init__.py:15`; profile `executor.md`; test `test_agent_markdown.py:32`. In-repo rule-name string (not DB/on-disk) → Phase A; producer + dict key + profile frontmatter + test in lockstep. |
| Scenario classes `InitialGoal`/`NestedGoal`/`NestedGoalFailure` + registry names `pipeline.initial_goal`/`pipeline.nested_goal[_failure]` | **A** | class | `InitialWorkflow`/`NestedWorkflow`/`NestedWorkflowFailure` / `pipeline.initial_workflow`/... | `scenarios/pipeline/{initial_goal,nested_goal}.py`; `pipeline/__init__.py`; `scenarios/__init__.py` (REGISTRY); `capacity/pack_catalog.py:76-87`; tests `test_scenario_suite_imports.py:133-135`, `test_context_message_scenarios.py`, `test_focused_scenarios.py:25`, `test_capacity_scenario_packs.py:84,85`. In-repo registry strings → Phase A. |
| `assert_recursive_goal_closed_before_parent_guard` hook + `name=` identity | **A** | function | `assert_recursive_workflow_closed_before_parent_guard` | `hooks/builtins.py:168,174-198,215`; tests `test_full_case_user_input.py:17,60`, `test_full_system_capacity_matrix.py:16,71`, `test_full_stack_adversarial.py:18,122`. In-repo hook name → Phase A. |

**Doc anchors (Phase C prose phase):**

| token | phase | kind | proposed | consumers |
|---|---|---|---|---|
| HTML anchors `#goal-start-workflow`/`#delegated-child-goals` + search-index/nav mirrors | **C** | metadata_value | `#workflow-start-workflow`/`#delegated-child-workflows` | `docs/architecture/task_center/lifecycle.html:46,48,82,83,162`; `assets/search-index.js`; `assets/nav.js`. Doc-only; regenerate `search-index.js`/`nav.js`. |

**KEEP-by-spelling (entity-axis field with NO `goal` word):**

| token | phase | kind | proposed | consumers |
|---|---|---|---|---|
| `requested_by_task_id` | none | field | **KEEP spelling** | `goal/state.py`; `db/models/goal.py:30`; router; mock + tests. Listed only to note it's serialized; effectively KEEP-by-spelling. |

---

## 3. Contract & migration decisions (the Phase-D contract policy)

For each cluster: the **semantic answer** (always: rename to workflow where the
token is the entity), the **coordinated work**, and the **LOCKED phase** (A or D —
no open recommendations remain; D1/D2 and the defaults resolve every prior "open
decision"). The critical objective-vs-entity calls are called out so they are not
blind-replaced.

> **OBJECTIVE → KEEP (never touch):** `<goal>` XML tag, `Goal.goal` text column,
> `goal` / `deferred_goal` DB columns, `goal_handoff`,
> `deferred_goal_for_next_iteration`, `iteration_goal`, and the **D2 planner-facing
> tokens** (`submit_plan_closes_goal`/`submit_plan_defers_goal`, `PLANNER_*_GOAL_PLAN`,
> `PlannerDefersWithoutDeferredGoal`, `context.goal_entry_minimal`,
> `closes_goal_terminal`). These are the workflow's *goal* / the plan FOR it and are
> correct to keep.
> **ENTITY → rename:** `goal_id`, `goals` table, `goal_start`, `goal_closure_report`,
> `waiting_goal`, `task_center_goal_id`. These are the *unit's* identity and become
> workflow — symbol renames in Phase A, persisted bytes in Phase D.

**(a) DB DDL — `goals` table + FK `goals.id` + `goal_id` column on `iterations` + `uq_iteration_goal_sequence`. [Phase D — MANDATORY second gated commit]**

Semantic: rename the persisted SCHEMA to `workflows` / `ForeignKey("workflows.id")`
/ `workflow_id` / `uq_iteration_workflow_sequence`. (The `GoalRecord`/`WorkflowStore`
**class/module/file** renames are Phase A — see (b); only the SCHEMA bytes are here.)
KEEP the `goal`/`deferred_goal` text columns.

There is NO Alembic — schema is `Base.metadata.create_all` in `db/engine.py`.
**VERIFIED engine.py facts (cite real lines; re-confirm at edit time):**
- `init_db_with_legacy_check(_engine)` runs at **line 302**, BEFORE
  `Base.metadata.create_all(_engine)` at **line 304**.
- `_rename_columns(_engine)` runs at **line 307**, AFTER `create_all`.
  `_RENAMED_COLUMNS` (line 71) is **column-only and scoped to `task_center_tasks`**;
  `_rename_columns` (line 184) only does `ALTER TABLE ... RENAME COLUMN`.
- The only `RENAME TO` in the file is the `__<table>_legacy` backup inside
  `_rebuild_sqlite_table` (line 158).
- `init_db_with_legacy_check` (line 97) raises only on
  `_LEGACY_TIER_TABLES = {missions, episodes, trials}` (line 94).

**Why a one-line "add goals→workflows to the legacy path" is WRONG:** `create_all`
(line 304) runs before `_rename_columns` (307). If we only added a column/table to
the post-`create_all` rename machinery, `create_all` would FIRST create a fresh,
empty `workflows` table from the renamed ORM model, after which nothing would ever
migrate the existing `goals` rows — silently splitting state across two tables
(exactly the failure `init_db_with_legacy_check` was written to prevent).

**Concrete design (REQUIRED):**
1. **Table rename, PRE-`create_all`.** Add a NEW module-level
   `_RENAMED_TABLES: dict[str, str] = {"goals": "workflows"}` and a
   `_rename_tables(engine)` helper that, guarded by
   `insp.has_table("goals") and not insp.has_table("workflows")`, runs
   `ALTER TABLE "goals" RENAME TO "workflows"` (works on both SQLite ≥3.25 and
   Postgres; preserves all rows). **Call it between line 302
   (`init_db_with_legacy_check`) and line 304 (`create_all`)** so the existing
   `goals` rows are carried into `workflows` BEFORE `create_all` would otherwise
   make an empty one. After the rename, `create_all` is a no-op for that table.
2. **Column rename, POST-`create_all` (reuse the existing machinery).** Add an
   `iterations` entry to `_RENAMED_COLUMNS` (line 71):
   `"iterations": {"goal_id": "workflow_id"}`. `_rename_columns` (307) then issues
   `ALTER TABLE "iterations" RENAME COLUMN "goal_id" TO "workflow_id"` for both
   dialects. **FK + unique-constraint handling:** on SQLite ≥3.25 a table rename
   auto-retargets referencing FKs and `RENAME COLUMN` updates the column reference
   inside the column-level FK and the `UniqueConstraint`; Postgres likewise follows
   the table rename and column rename. The CONSTRAINT NAME stays the legacy
   `uq_iteration_goal_sequence` (cosmetic drift vs the ORM's
   `uq_iteration_workflow_sequence`). **Decision: accept the cosmetic name drift**
   (the constraint still enforces correctly; matching the ORM `__table_args__` keeps
   fresh DBs correct). If an exact name match on existing DBs is required, route
   `iterations` through the `engine.py:240` SQLite-rebuild branch
   (`_rebuild_sqlite_table`, line 128) instead — call this out at edit time; do NOT
   leave it silent.
3. **Stale-split guard, inside `init_db_with_legacy_check` (line 302, PRE-`create_all`).**
   Add a NEW conditional branch to `init_db_with_legacy_check` that raises ONLY when
   **both** `goals` AND `workflows` tables already exist (a true split state the
   auto-rename cannot safely resolve). **Do NOT append `goals` to `_LEGACY_TIER_TABLES`**
   (line 94) — that set raises on *mere presence*, which is wrong for `goals` (it
   legitimately exists pre-rename on every un-migrated DB). The new branch is a
   distinct both-exist check.
   **Ordering (non-contradictory):** the guard inside `init_db_with_legacy_check`
   (line 302) runs first, then `_rename_tables` (step 1, line 302.5), then `create_all`
   (line 304). Because `_rename_tables` is existence-guarded
   (`has_table("goals") and not has_table("workflows")`), the order is immaterial across
   all three cases: (clean) guard sees only `goals` → does not raise; `_rename_tables`
   renames `goals→workflows`. (split) guard sees both → raises before any rename.
   (already-migrated) neither fires. So the guard-first placement is safe and the doc
   is self-consistent.
4. Update `test_migration_drops_legacy_table.py:42` (`"goals"` → `"workflows"`).

**Data-safety gates (Phase D verify):** (i) `test_goal_store.py` (renamed
`test_workflow_store.py`) round-trips the renamed `workflows` table green;
(ii) a fresh-DB-creates-`workflows` test (init on an empty DB → assert `workflows`
table exists, `goals` does not, and a row inserts/reads back).

**Accepted-scope assumption (NOT a migration — recorded explicitly, not silent):**
The Phase D DDL renames *schema* (the `goals` table + `iterations.goal_id` column),
NOT pre-existing persisted *row values* or historical on-disk artifacts. Two concrete
cases are intentionally left un-migrated:
- **`"waiting_goal"` row values in `task_center_tasks.status`.** After Phase D the
  code emits/compares `"waiting_workflow"`; a stranded `"waiting_goal"` status row from
  a pre-Phase-D run would silently mismatch. This is accepted because `waiting_goal` is
  a *transient in-run* task status (a task is `waiting_*` only mid-run, on an ephemeral
  test-harness DB), so no durable row carries it across the rename.
- **Historical `sandbox_events.jsonl` keys** (`goal_id`, `task_center_goal_id`) from
  prior runs stay as-written; only newly-emitted events use `workflow_id`.
This matches the project's existing dev-DB practice: the prior `missions→goals` rename
used a legacy-check + drop (`init_db_with_legacy_check`/`drop_legacy_tier_tables`), not
a row-value migration. If a durable production DB ever needs value-level migration,
that is a separate, out-of-scope effort.

**(b) Facade `_EXPORTS` string keys + module-path strings + DB class/module/file renames. [Phase A — in-repo Python only]**
Semantic: rename the public names (`Goal`→`Workflow`, `GoalRecord`→`WorkflowRecord`,
`GoalStore`→`WorkflowStore`, ...) AND the module-path strings
(`task_center.goal.state`→`task_center.workflow.state`, `db.stores.goal_store`→
`db.stores.workflow_store`), AND rename the files `db/models/goal.py→workflow.py`
and `db/stores/goal_store.py→workflow_store.py` (defaults LOCKED). Coordinated work:
LSP-rename the symbols, `git mv` the files/package, edit `_EXPORTS` keys + values +
the `TYPE_CHECKING` block + `__all__`, and every `from task_center import Goal/...`
site. Renaming a Python class/import path/file changes no persisted byte → Phase A.

**(c) `goal_id` everywhere (serialized key + `ScopeField` Literal + `require_field("goal_id")` + `ContextRefs`/`ContextPacket` + `AuditNode.goal_id`/`NodeId` + `task_center_goal_id`). [Phase D — atomic, default LOCKED]**
Semantic: `workflow_id` / `task_center_workflow_id`. This is the deepest
cross-system contract and is renamed atomically in Phase D alongside the DB column
(a): the DB column, the `ScopeField` Literal + `require_field` string, the persisted
`ContextPacket` refs, the `SandboxCaller.audit_fields()` + `_CALLER_AUDIT_FIELDS`
wire tuple, the engine audit-stream metadata read, the
`ToolExecutionContext._TYPED_FIELDS` allowlist, `AuditNode.goal_id` (def in shared
`audit/base.py`), `NodeId`, the persisted `sandbox_events.jsonl` log, the mock
`probes.py`/`runner.py` string reads, and 4+ sandbox/audit tests. Renaming `goal_id`
in isolation while keeping the column would only create a translation seam, so it is
all-in-Phase-D.

**(d) Closure-report payload `"goal_closure_report"` (key + `submission_kind` value) + `.goal_id` payload key. [VALUE → Phase D; method/DTO → Phase A]**
Semantic: `"workflow_closure_report"` / `workflow_id`. **Silent-break hazard:** the
method `apply_goal_closure_report` and the `GoalClosureReport*` DTOs rename freely in
Phase A (no persisted byte), but the payload VALUE STRING `"goal_closure_report"` is
read by mock `runner.py:1968` and asserted in two tests with NO import error if
missed — that is a cross-process submission byte and migrates in Phase D. Coordinated
work (Phase D): producer `orchestrator.py:204,205` + the two test asserts + the mock
read, in lockstep. Bundle with (e).

**(e) Persisted audit/summary VALUES: `goal_start`, `waiting_goal` (value), `request_recursive_goal:`, `recursive_goals`, the capacity/full-stack on-disk summary `goals` key, `goal.json`/`goal_<NN>` artifact layout. [Phase D]**
Semantic: all → `workflow_*`. Each is a producer + ≥1 consumer (mock runner /
scenario expect-lists / on-disk artifact). Update producer and every
string-match/glob in lockstep; `waiting_goal` is a persisted task-status string
(needs the status migration). The ENTITY EventType **members** (`GOAL_*`/
`RECURSIVE_GOAL_*`) are NOT here — both their NAME and value move in Phase A (verified
no persisted/string-match consumer; see §3(f)). The in-memory `graph_summary["goals"]`
key is NOT here — it is Phase A (verified not on-disk).

**(f) In-repo tool/trigger/scenario/hook NAMES + ENTITY EventType members (NAME + value). [Phase A]**
`request_goal_after_edit` rule name, `InitialGoal`/`NestedGoal`/`NestedGoalFailure` +
registry names, `assert_recursive_goal_closed_before_parent_guard`, ENTITY EventType
**members edited whole** (`GOAL_* = "goal_*"`, `RECURSIVE_GOAL_* = "recursive_goal_*"`
→ `WORKFLOW_*`/`RECURSIVE_WORKFLOW_*` member+value; `WAITING_GOAL` member name only —
its `"waiting_goal"` value is the persisted task-status byte → Phase D). Semantic:
rename to workflow. They have no DB/on-disk persistence — only in-repo strings (and the
event values, verified to have no persisted/string-match consumer) updated atomically
and validated by the Phase A pytest sweep, so they move in Phase A. (Note: the
`closes_goal_terminal` check key is NOT here — it is a D2 KEEP.)

**(g) Planner-facing tokens — RESOLVED to KEEP per D2 (LOCKED, no longer ambiguous).**
`submit_plan_closes_goal` / `submit_plan_defers_goal` tool names (+ constants + DTOs
+ dirs), `PLANNER_COMPLETES_GOAL_PLAN` / `PLANNER_DEFERS_GOAL_PLAN` EventType members
AND values, `PlannerDefersWithoutDeferredGoal`, `context.goal_entry_minimal`, and the
`closes_goal_terminal` check key all **KEEP `goal`** — they read as the OBJECTIVE the
plan pursues, not the unit. There is NO Phase D for them. This also resolves the
`closes_goal_terminal`/tool-name coherence concern: the check key stays `goal`
because the tool it checks (`submit_plan_closes_goal`) stays `goal`.

---

## 4. Phased, ordered plan

**Atomicity:** A Python symbol + package rename breaks every importer at once;
there is NO green tree mid-sweep. **Phases A1–A5 are ONE uncommitted atomic sweep**
(tree expected RED throughout); the single commit (**commit 1**) happens only after
A5 is green. The **serialized-contract / DB migration is Phase D — a clearly
separated, explicitly-gated MANDATORY second commit (commit 2) in the SAME effort**
(D1). Phase D is NOT optional and NOT deferrable: the effort is complete only when
both commits land. Prose/docs are Phase C. Stage explicit paths only; verify against
HEAD before each commit (parallel agents edit concurrently — the dirty
`probe_bridge.py` / `mock_event_source` files are unrelated, do not touch).

**Commit/phase order:** Phase A (commit 1, symbols+package) → Phase C (prose/docs,
may fold into commit 1 or land separately) → Phase D (commit 2, DB + persisted
VALUES + on-disk artifacts). Phase D is gated on Phase A being green but is REQUIRED.

**Recommended tooling:** use **LSP-driven rename** (`mcp__cclsp__rename_symbol` /
`lsp_rename`) for every PascalCase axis symbol (`Goal`, `GoalStatus`, `GoalOrigin`,
`GoalOriginKind`, `GoalStarter`, `StartedGoal`, `GoalLifecycle`, `GoalClosureReport*`,
`GoalStoreProtocol`, `GoalStore`, `GoalRecord`, `GoalClosureReportRouter`,
`_PreparedGoalOrigin`, `AttemptDelegatedGoalParentTask`, scenario classes). LSP
follows the call graph and updates ALL importers (catches `recorder.py` /
`db/models/__init__.py` / conftest consumers a grep misses) and — critically —
does NOT disturb KEEP string literals (`"<goal>"`, `"goals"`, `goal_handoff`).

### Phase 0 — Baseline & safety net (no edits)
- `.venv/bin/pytest backend/tests/unit_test/test_task_center -q`
- `.venv/bin/pytest backend/src/task_center_runner/tests/mock/contracts -q`
- Record KEEP-string inventory: `grep -rn '<goal>\|goal_handoff\|deferred_goal_for_next_iteration\|"tag":"goal"' backend/src`
- **Verify:** baseline green; note pre-existing failures from parallel agents.

### Phase A — Atomic symbol + package sweep (ONE uncommitted RED window)

**A1 — intra-package axis symbols, THEN package move (ordering matters).**

**Ordering rule (suggested fix):** LSP-rename the SYMBOLS **first**, while the
package still resolves at `task_center.goal` so the LSP reference graph is intact and
the rename updates ALL importers across the repo. **Only then** `git mv` the package,
**then** fix the module-path STRINGS (`_EXPORTS`, `db.stores` import paths). Doing the
`git mv` first would break the import graph and the LSP may under-rename (it can't
follow references through an unresolvable module path).

- **(1) LSP-rename axis symbols** (2A + 2C-facade-cluster), package still at
  `task_center.goal`: `Goal`→`Workflow`, `GoalStatus`→`WorkflowStatus`,
  `GoalOrigin(Kind)`→`WorkflowOrigin(Kind)`, `GoalClosureReport*`→
  `WorkflowClosureReport*`, `GoalClosureDelivery*`, `GoalClosureCallback`,
  `GoalLifecycle`, `GoalStarter`, `StartedGoal`, `GoalClosureReportRouter`,
  `GoalRecord`→`WorkflowRecord`, `GoalStore`→`WorkflowStore`,
  `GoalStoreProtocol`→`WorkflowStoreProtocol`, `_PreparedGoalOrigin`,
  `AttemptDelegatedGoalParentTask`, `nested_goal_depth`, scenario classes. Methods:
  `create_goal`/`close_goal`/`_require_goal`/`_build_goal_lifecycle`,
  `apply_goal_closure_report`, `child_outcomes_for_goal`,
  `parent_task_for_delegated_goal`, `mark_waiting_goal`,
  `restore_running_after_failed_goal_start`, `assert_goal_open`,
  `list_for_goal`, `assert_iteration_id_unique_in_goal`,
  `_nested_goal_depth_gt_1`, `goal_store` DI attr. KEEP: `goal:str` field, `goal=`
  kwargs (objective), `goal_id` params (→ Phase D), enum VALUES, `WAITING_GOAL.value`
  references (→ Phase D), `<goal>`/`iteration_goal`, all D2 KEEP tokens.
- **(2) `git mv backend/src/task_center/goal` → `backend/src/task_center/workflow`**
  (`__init__.py`, `state.py`, `ancestry.py`, `lifecycle.py`, `starter.py`,
  `closure_report_router.py`). Add a two-register docstring to
  `workflow/__init__.py` + `workflow/state.py` stating: *axis = Workflow; the tokens
  `goal`/`<goal>`/`Goal.goal` are the OBJECTIVE the workflow pursues and intentionally
  stay; entity tokens `goal_id`/`goals`/`goal_start` migrate to `workflow*` in the
  mandatory Phase D contract commit.*
- **(3) Fix module-path STRINGS** that the LSP/mv did not catch: intra-package
  imports → `task_center.workflow.*`; `_EXPORTS` module-path strings; `db.stores`
  import paths.
- **(4) Pull entity-prose into Phase A (Blocking #1) — derive the edit set from the
  gate, do not rely on the enumerated list.** After the symbol renames (1)–(3), run
  `grep -rnE '\bGoal\b' backend/src backend/tests | grep -v __pycache__`; the residue
  (matches MINUS the symbol-rename set already handled MINUS the two KEEP DTOs
  `SubmitPlanClosesGoalInput`/`SubmitPlanDefersGoalInput`, which the `\bGoal\b` pattern
  does not even match) IS exactly the bare-`Goal` English-word DOCSTRING/COMMENT prose
  to edit. This makes "gate satisfiable at Phase A close" true *by construction*. The
  2A list (moved `workflow/*.py`, `db/models/workflow.py`, `db/stores/workflow_store.py`,
  `_core/primitives.py`, `entry/bootstrap.py`, `_core/invariants.py`,
  `generator_summaries.py`, `terminal_tool_routing.py`, scenario docstrings) is the
  expected residue — treat any extra match as an additional prose edit, not a surprise.
  Per-sentence: keep `goal` where it means the objective. (Only `docs/architecture/**.html`
  + `CLAUDE.md` prose stay for Phase C — outside the gate's `backend/src`/`backend/tests`
  path scope.)

**A2 — ripple `task_center/` consumers + cross-package store class.**
- `task_center/__init__.py` `_EXPORTS` keys + module-path strings + `TYPE_CHECKING`.
- `_core/persistence.py` (`GoalStoreProtocol`→`WorkflowStoreProtocol`),
  `terminal_tool_routing.py` (`_nested_goal_depth_gt_1`→`_nested_workflow_depth_gt_1`),
  `invariants.py` (`assert_goal_open`), `entry/bootstrap.py`,
  `context_engine/recipes/iterations.py` (`Goal`→`Workflow` type),
  `context_engine/core.py` (`goal_store`→`workflow_store` attr + recipe call sites),
  `attempt/{deps,launch,orchestrator,orchestrator_registry}.py`
  (`AttemptDelegatedGoalParentTask`, `apply_goal_closure_report`,
  `parent_task_for_delegated_goal`, `mark_waiting_goal`,
  `restore_running_after_failed_goal_start`, `child_outcomes_for_goal`).
  KEEP `goal_id`, `WAITING_GOAL.value`, `"goal_closure_report"` string (→ Phase D).
- `git mv db/stores/goal_store.py → db/stores/workflow_store.py` (`GoalStore`→
  `WorkflowStore`; imports → `task_center.workflow.state`),
  `git mv db/models/goal.py → db/models/workflow.py` (`GoalRecord`→`WorkflowRecord`;
  KEEP `__tablename__="goals"`, `goal` + `requested_by_task_id` columns → unchanged;
  table name → Phase D), `db/models/__init__.py`, `db/stores/__init__.py`
  (`_EXPORTS` `GoalStore`→`WorkflowStore` key + `db.stores.goal_store`→
  `db.stores.workflow_store` path). File renames are Phase A (default LOCKED).

**A3 — ripple tests that import axis symbols** (per v1's 16-importer list):
`test_domain/test_goal_dto.py`, `test_ancestry.py`,
`test_agent_launch/test_terminal_tool_router.py` (5 monkeypatch strings →
`_nested_workflow_depth_gt_1`), `test_task_center/conftest.py`,
`test_tools/conftest.py`, `test_persistence/test_goal_store.py`,
`test_context_engine/test_role_context_matches_diagram.py`,
`test_benchmarks/test_sweevo_audit_recorder.py`,
`test_tools/test_submission_terminal_routing.py`, `test_lifecycle/*`,
`test_saga_inline_equivalence.py`. KEEP `<goal>`/`goal=`/`goal_id` fixture data and
all `WAITING_GOAL.value` / `"goal_closure_report"` asserts (→ Phase D).

**A4 — runner production + tests** (in-repo string-match + symbols; persisted bytes → Phase D):
`task_center_runner/audit/recorder.py` (`GoalRecord`→`WorkflowRecord` types +
`_serialize_goal`/`_handle_goal`/`_ensure_goal_dir`/`_goal_dir`/`_goal_seq_counter`
internal SYMBOLS → `*_workflow`; KEEP `record.goal` payload key; KEEP the written
`goal.json`/`goal_<NN>` filenames → Phase D),
`agent/mock/{runner,probes,scenario_adapter,scenario_loop_runner}.py` (axis symbols
+ `goal_store`→`workflow_store`; KEEP every persisted prompt/metadata VALUE string),
`core/{runner,engine,stores}.py`, scenario helper `goal_origin.py`→`workflow_origin.py`,
`ScenarioContext.goal`→`.workflow`, scenario classes/hook guard/notification rule,
**ENTITY EventType members edited WHOLE** (`GOAL_* = "goal_*"`→`WORKFLOW_* = "workflow_*"`,
`RECURSIVE_GOAL_* = "recursive_goal_*"`→`RECURSIVE_WORKFLOW_* = "recursive_workflow_*"`;
`WAITING_GOAL` member name → `WAITING_WORKFLOW`, but KEEP its `"waiting_goal"` VALUE
→ Phase D) per §3(f) (in-repo `.name`/member references + scenario expect-lists,
validated by the sweep; these event values have NO persisted/string-match consumer —
verified), the in-memory `graph_summary["goals"]` RunReport key → `"workflows"` + its
~14 test asserts (verified not on-disk → Phase A). KEEP (→ Phase D): the on-disk
capacity/full-stack summary `goals`/`recursive_goals` keys, `goal.json`/`goal_<NN>`
filenames, `goal_start`, `waiting_goal` VALUE, `task_center_goal_id`,
`request_recursive_goal:`, `"goal_closure_report"` VALUE. KEEP (D2, no rename):
`submit_plan_*_goal` tool names, `PLANNER_*_GOAL_PLAN` (member+value),
`PlannerDefersWithoutDeferredGoal`, `closes_goal_terminal`, `context.goal_entry_minimal`.

**A5 — submission-context method + tools touch:**
`tools/submission/context/executor.py` (`start_delegated_goal`→`start_delegated_workflow`),
`submit_execution_handoff.py` call site, `test_tools/*` stubs. KEEP `goal_handoff`.

**Verify A (commit 1 only after green) — the gate is SATISFIABLE at Phase A close
because A1(4) pulled the `.py` entity-prose into Phase A:**
- `.venv/bin/ruff check backend/src backend/tests`
- `.venv/bin/pytest backend/tests/unit_test/test_task_center backend/tests/unit_test/test_tools backend/tests/unit_test/test_benchmarks -q`
- `.venv/bin/pytest backend/src/task_center_runner/tests/mock/contracts backend/src/task_center_runner/tests/mock/task_center/test_correctness.py -q`
- Grep gates §5 (enumerated PascalCase axis set → 0; renamed snake_case set → 0;
  `goal_store` → 0; `task_center.goal` → 0; `goal/` dir absent; KEEP + D2-KEEP
  strings present; persisted-VALUE strings STILL present — they move in Phase D).

### Phase C — Prose / docstrings / prompts / docs (may fold into commit 1 or land separately)
- Reframe `submit_execution_handoff/prompt.py` prose; `_terminals/registry.py` prose;
  `docs/architecture/**` (Workflow → Iteration → Attempt; axis-symbol mentions;
  `data-last-reviewed-commit` + `data-evidence-paths` → `task_center/workflow/*`);
  `CLAUDE.md` model phrase + anchor paths. **Per-sentence:** keep `goal` where it means
  the objective. (The `.py` entity-prose is NOT here — it was pulled into Phase A
  A1(4); Phase C is `docs/*.html` + `CLAUDE.md` prose only.)
- HTML anchor ids `#goal-start-workflow`/`#delegated-child-goals` → workflow form
  to mirror Phase D's section renames (regenerate `search-index.js`/`nav.js`).
- **Verify:** `.venv/bin/ruff check backend/src/tools backend/src/db`;
  `.venv/bin/pytest backend/tests/unit_test/test_tools -q`; doc grep §5.

### Phase D — MANDATORY second gated commit: serialized-contract / DB migration (commit 2)

**Required, not deferrable** (D1). Gated on Phase A being green; the effort is
complete only when this commit lands. Bundles all persisted VALUES/keys/DB bytes:

- **DB DDL (per §3(a) concrete design):**
  1. Add `_RENAMED_TABLES = {"goals": "workflows"}` + a `_rename_tables(engine)` helper
     (`ALTER TABLE "goals" RENAME TO "workflows"`, guarded `has_table("goals") and not
     has_table("workflows")`), called **between `init_db_with_legacy_check` (line 302)
     and `create_all` (line 304)** — PRE-`create_all` so existing rows are preserved.
  2. Add `"iterations": {"goal_id": "workflow_id"}` to `_RENAMED_COLUMNS` (line 71);
     the existing post-`create_all` `_rename_columns` (line 307) renames the column for
     both dialects. FK + `uq_iteration_goal_sequence` auto-follow the rename (SQLite
     ≥3.25 / Postgres); accept the cosmetic constraint-name drift (or route `iterations`
     through `_rebuild_sqlite_table` line 128 if exact-name parity is required — decide
     at edit time, do not leave silent).
  3. Add `goals` (stale pre-rename name) to `init_db_with_legacy_check` (line 97) — raise
     ONLY when BOTH `goals` and `workflows` exist (true split); `_rename_tables` runs
     first and collapses the clean single-table case.
  4. Update `test_migration_drops_legacy_table.py:42` (`"goals"`→`"workflows"`).
  KEEP `goal`/`deferred_goal` text columns.
- **`goal_id` everywhere (atomic, §3(c)):** `ScopeField` Literal + `require_field` +
  `ContextRefs`/`ContextPacket` + `AuditNode`/`NodeId` (`goal_seq`→`workflow_seq`) +
  `task_center_goal_id` + `_TYPED_FIELDS` + `_CALLER_AUDIT_FIELDS` + `sandbox_events.jsonl`
  + mock `probes.py`/`runner.py` reads + 4+ sandbox/audit tests + `AgentLaunch.goal_id`
  + the `goal_id` store params + `delegated_goal_id` payload key + `GoalClosureReport.goal_id`
  payload key.
- **Persisted VALUE strings:** `"waiting_goal"`, `"goal_start"`, `"goal_closure_report"`
  (payload key + `submission_kind`), `request_recursive_goal:`. (NOT the ENTITY
  EventType event values `"goal_started"`/`"recursive_goal_*"` — those have no
  persisted/string-match consumer and move WHOLE in Phase A; see §3(f).)
- **On-disk artifacts:** `goal.json` filename + `goal_<NN>_<id>` audit dir name;
  the capacity/full-stack on-disk summary `goals`/`recursive_goals` keys.
- **Doc anchors** (§2C doc-anchors row) to mirror the renamed sections.
- **Verify (data-safety gates):** full suite + `test_workflow_store.py` (renamed
  `test_goal_store.py`) round-trip on the renamed `workflows` table + a NEW
  fresh-DB-creates-`workflows` test (empty DB → `workflows` exists, `goals` does not,
  row inserts/reads back) + audit-recorder + sandbox audit/contract tests + Phase D
  grep gates (`goal_id`/`task_center_goal_id`/`waiting_goal`/`goal_start`/
  `goal_closure_report`/`goal.json`/on-disk `"goals"` → 0 in renamed scope).

---

## 5. Acceptance criteria

**Do NOT use a blanket `grep Goal → 0`** — it is unsatisfiable: mixed-case KEEPs
survive. Enumerate the real renamed set; add proof-of-KEEP greps. **With A1(4)
pulling the `.py` entity-prose into Phase A, the enumerated allowlist gate below is
SATISFIABLE at Phase A close** (no bare-`Goal` English-word prose lingers in
`backend/src`/`backend/tests` `.py` files; only the enumerated PascalCase identifiers
are matched, and KEEP identifiers survive by word-boundary — see the NOTE).

**KEEP-survivor classification (must remain after Phase A):**
- Scenario classes `NestedGoal`/`InitialGoal`/`NestedGoalFailure`,
  `AttemptDelegatedGoalParentTask`, `_PreparedGoalOrigin` → **RENAMED in Phase A**
  (in-repo only) → gone after A.
- `PlannerDefersWithoutDeferredGoal` → **KEEP** (D2, objective). Tool DTOs
  `SubmitPlanClosesGoalInput`/`SubmitPlanDefersGoalInput` → **KEEP** (D2; DTO class
  name tracks the kept tool name). These survive the gate by word-boundary (`\bGoal\b`
  does not match inside `...ClosesGoalInput` / `...DeferredGoal`).

**Phase A grep gates (after the symbol sweep):**
- PascalCase axis-symbol set → **0**:
  `grep -rnE '\b(Goal|GoalStatus|GoalOrigin|GoalOriginKind|GoalClosureReport|GoalClosureDelivery\w*|GoalStarter|StartedGoal|GoalLifecycle|GoalClosureCallback|GoalClosureReportRouter|GoalStoreProtocol|GoalStore|GoalRecord|_PreparedGoalOrigin|AttemptDelegatedGoalParentTask|InitialGoal|NestedGoal|NestedGoalFailure)\b' backend/src backend/tests | grep -v __pycache__`
  **SATISFIABLE at Phase A close** (bare `\bGoal\b` no longer matches docstring prose —
  A1(4) edited it; the only `Goal` tokens remaining are KEEPs that the `\b...\b`
  pattern does not match: `SubmitPlanClosesGoalInput`/`SubmitPlanDefersGoalInput`,
  `PlannerDefersWithoutDeferredGoal`, and the objective tokens `Goal.goal` field
  reference uses lowercase `goal`).
- Renamed snake_case method set → **0**:
  `grep -rnE '\b(nested_goal_depth|_nested_goal_depth_gt_1|assert_goal_open|create_goal|close_goal|_require_goal|_build_goal_lifecycle|apply_goal_closure_report|start_delegated_goal|parent_task_for_delegated_goal|mark_waiting_goal|restore_running_after_failed_goal_start|child_outcomes_for_goal|is_entry_origin_goal|is_recursive_goal|list_for_goal|assert_iteration_id_unique_in_goal)\b' backend/src backend/tests | grep -v __pycache__`
  (Excludes KEEPs `deferred_goal_for_next_iteration`, `goal_handoff`,
  `recursive_handoff_goal`, `assert_predecessor_has_deferred_goal_for_next_iteration`,
  the D2 tool/check tokens, and the Phase-D-gated `goal_id`-family params.)
- `goal_store` DI attr/fixture → **0** (renamed to `workflow_store` in Phase A):
  `grep -rn '\bgoal_store\b' backend/src backend/tests | grep -v __pycache__` → **0**
  (31 src + ~432 test occurrences renamed; this gate catches a missed fixture).
- Package gone: `test -d backend/src/task_center/goal && echo FAIL || echo OK` → **OK**;
  `grep -rn 'task_center\.goal' backend/src backend/tests | grep -v __pycache__` → **0**.

**Proof-of-KEEP (objective tokens must STILL be present after Phase A):**
- `grep -rn '<goal>\|"tag": *"goal"\|source_kind="goal"' backend/src/task_center/context_engine` → present.
- `grep -rn 'goal_handoff\|deferred_goal_for_next_iteration' backend/src/tools backend/src/task_center` → present.
- `grep -rn 'goal: str\|goal=goal\|\.goal\b' backend/src/task_center/workflow/state.py` → `Workflow.goal` field present.
- Mock string-matches untouched: `grep -rn '"<goal>" in prompt\|"goal": "<goal>"' backend/src/task_center_runner/agent/mock` → present.
- D2-KEEP tokens present: `grep -rn 'submit_plan_closes_goal\|submit_plan_defers_goal\|PLANNER_COMPLETES_GOAL_PLAN\|PlannerDefersWithoutDeferredGoal\|closes_goal_terminal\|goal_entry_minimal' backend/src` → present (KEEP per D2).
- **Persisted-VALUE strings STILL present after Phase A** (they move only in Phase D —
  the byte hasn't changed yet): `grep -rn 'waiting_goal\|goal_start\|goal_closure_report\|task_center_goal_id\|goal_id\|goal\.json\|request_recursive_goal' backend/src` → present.
  (NOTE: the in-memory `graph_summary["goals"]` RunReport key, the ENTITY EventType
  members (`GOAL_*`/`RECURSIVE_GOAL_*`, **NAME and value**), and the `WAITING_GOAL`
  member NAME are renamed in Phase A — do NOT expect those to survive. Only the
  genuinely-persisted VALUE strings + on-disk artifacts + DB column do.)
- **ENTITY event values renamed in Phase A** (no persisted consumer):
  `grep -rn '"goal_started"\|"goal_completed"\|"goal_requested"\|"recursive_goal_requested"\|"recursive_goal_completed"' backend/src | grep -v __pycache__` → **0** after Phase A;
  the `workflow_*` forms present at `audit/events.py`.
- `goal` DB columns unchanged after Phase A: `grep -rn '__tablename__ = "goals"\|goal: Mapped\|deferred_goal: Mapped' backend/src/db/models` → present (column/table migrate in Phase D).

**Phase D grep gates (only after the mandatory commit 2):**
- `grep -rnE '\b(goal_id|task_center_goal_id|delegated_goal_id)\b' backend/src backend/tests | grep -v __pycache__` → **0** (renamed to `workflow_id`/`task_center_workflow_id`/`delegated_workflow_id`).
- `grep -rn '"waiting_goal"\|"goal_start"\|"goal_closure_report"\|goal\.json\|request_recursive_goal' backend/src backend/tests | grep -v __pycache__` → **0**. (The ENTITY event values `"goal_started"`/`"recursive_goal_*"` are NOT in this gate — they already went to 0 in Phase A.)
- DB: `grep -rn '"goals"\|ForeignKey("goals' backend/src/db backend/tests` → **0** (table is `workflows`); on-disk capacity summary `graph["goals"]` → `workflows`.
- `test_workflow_store.py` round-trips the renamed `workflows` table green; the new fresh-DB test asserts `workflows` exists and `goals` does not.

**Doc gates (Phase C):** `grep -rn 'Goal -> Iteration\|GoalStarter\|nested_goal_depth' CLAUDE.md docs/architecture` → **0** (excluding flagged historical `docs/task_center_harness_and_context_engine.html`); each touched page has updated `data-last-reviewed-commit`.

---

## 5.1 Naming-occurrence rename checklist (acceptance)

Per-token acceptance checklist for the `goal → workflow` rename, grounded in the live
tree: case-insensitive `goal` ≈ **3,580 line-occurrences** across `backend/src`
(task_center 463, db 88, tools 142, task_center_runner 675, engine 1, sandbox 3),
`backend/tests` (1,417), and `docs` (795), collapsing to the **~80 distinct tokens**
below. Counts are current-tree occurrence counts at plan time; line numbers re-derived
at edit time (trust LSP + the §5 grep gates over counts). Check each item when its
rename — or KEEP verification — is complete.

### Files & directories

- [ ] `backend/src/task_center/goal/` → `backend/src/task_center/workflow/` (package; moves `__init__.py`, `state.py`, `ancestry.py`, `lifecycle.py`, `starter.py`, `closure_report_router.py`)
- [ ] `backend/src/db/models/goal.py` → `backend/src/db/models/workflow.py`
- [ ] `backend/src/db/stores/goal_store.py` → `backend/src/db/stores/workflow_store.py`
- [ ] `backend/src/task_center_runner/scenarios/_scenario_helpers/goal_origin.py` → `workflow_origin.py`
- [ ] `backend/src/task_center_runner/scenarios/pipeline/initial_goal.py` → `initial_workflow.py`
- [ ] `backend/src/task_center_runner/scenarios/pipeline/nested_goal.py` → `nested_workflow.py`
- [ ] `backend/src/tools/submission/notification_triggers/request_goal_after_edit.py` → `request_workflow_after_edit.py`
- [ ] `backend/tests/unit_test/test_task_center/test_domain/test_goal_dto.py` → `test_workflow_dto.py` (cosmetic — match symbol)
- [ ] `backend/tests/unit_test/test_task_center/test_lifecycle/test_goal_lifecycle.py` → `test_workflow_lifecycle.py` (cosmetic)
- [ ] `backend/tests/unit_test/test_task_center/test_lifecycle/test_phase04_goal_request_start.py` → `test_phase04_workflow_request_start.py` (cosmetic)
- [ ] `backend/tests/unit_test/test_task_center/test_persistence/test_goal_store.py` → `test_workflow_store.py` (cosmetic)
- [ ] UNCHANGED (D2 keep): `scenarios/planner_validation/defers_without_deferred_goal.py`; `tools/submission/planner/submit_plan_closes_goal/`; `tools/submission/planner/submit_plan_defers_goal/`
- [ ] UNCHANGED: `task_center/iteration/`, `task_center/attempt/` (sub-axes a Workflow owns)

### Phase A — RENAME → `Workflow` (PascalCase, pure in-process)

- [ ] `Goal` (90) → `Workflow`
- [ ] `GoalOrigin` (63) → `WorkflowOrigin`
- [ ] `GoalStatus` (62) → `WorkflowStatus` (enum *values* `open/succeeded/failed/cancelled` KEEP)
- [ ] `GoalOriginKind` (38) → `WorkflowOriginKind` (values `entry/task` KEEP)
- [ ] `GoalClosureReport` (27) → `WorkflowClosureReport`
- [ ] `GoalStarter` (26) → `WorkflowStarter`
- [ ] `GoalRecord` (22) → `WorkflowRecord` (ORM class; `__tablename__` stays `goals` → Phase D)
- [ ] `GoalStore` (21) → `WorkflowStore`
- [ ] `GoalLifecycle` (18) → `WorkflowLifecycle`
- [ ] `StartedGoal` (13) → `StartedWorkflow`
- [ ] `NestedGoalFailure` (11) → `NestedWorkflowFailure` (scenario class)
- [ ] `NestedGoal` (11) → `NestedWorkflow` (scenario class)
- [ ] `GoalClosureReportRouter` (11) → `WorkflowClosureReportRouter`
- [ ] `GoalStoreProtocol` (10) → `WorkflowStoreProtocol`
- [ ] `GoalClosureDeliveryResult` (10) → `WorkflowClosureDeliveryResult`
- [ ] `InitialGoal` (9) → `InitialWorkflow` (scenario class)
- [ ] `GoalClosureDeliveryStatus` (4) → `WorkflowClosureDeliveryStatus`
- [ ] `AttemptDelegatedGoalParentTask` (4) → `AttemptDelegatedWorkflowParentTask`
- [ ] `GoalClosureCallback` (3) → `WorkflowClosureCallback`
- [ ] `GoalStart` (1) → `WorkflowStart` (verify at edit — distinct from `GoalStarter`)

### Phase A — RENAME → `workflow` (snake_case symbols / methods / local vars)

- [ ] `goal_store` (35) / `_goal_store` (9) → `workflow_store` / `_workflow_store` (gated by §5 `goal_store → 0`)
- [ ] `is_recursive_goal` (13) → `is_recursive_workflow`
- [ ] `is_entry_origin_goal` (12) → `is_entry_origin_workflow`
- [ ] `assert_recursive_goal_closed_before_parent_guard` (12) → `assert_recursive_workflow_closed_before_parent_guard` (hook name)
- [ ] `goal_status` (10, lowercase var/attr) → `workflow_status` (verify)
- [ ] `started_goal` (7) → `started_workflow`
- [ ] `current_goal_id` (6) / `seen_goal_ids` (3) / `open_goals` (3) / `current_goal` (4) / `first_goal` (3) / `final_goal` (2) / `created_goal` (5) → `current_workflow_id` / `seen_workflow_ids` / `open_workflows` / `current_workflow` / `first_workflow` / `final_workflow` / `created_workflow` (local vars)
- [ ] `assert_goal_open` (6) → `assert_workflow_open`
- [ ] `apply_goal_closure_report` (6, method) → `apply_workflow_closure_report` (payload **value** `"goal_closure_report"` → Phase D)
- [ ] `parent_task_for_delegated_goal` (4) → `parent_task_for_delegated_workflow`
- [ ] `make_goal_request_after_edit_reminder` (4) → `make_workflow_request_after_edit_reminder`
- [ ] `nested_goal_depth` (4) / `_nested_goal_depth_gt_1` → `nested_workflow_depth` / `_nested_workflow_depth_gt_1` (5 monkeypatch target strings lockstep)
- [ ] `child_outcomes_for_goal` (4) → `child_outcomes_for_workflow`
- [ ] `assert_iteration_id_unique_in_goal` (4) → `assert_iteration_id_unique_in_workflow`
- [ ] `list_for_goal` (5) → `list_for_workflow`
- [ ] `_require_goal` (4) / `_build_goal_lifecycle` (2) / `goal_lifecycle` (3, var) → `_require_workflow` / `_build_workflow_lifecycle` / `workflow_lifecycle`
- [ ] `_recursive_goal_count` (4) → `_recursive_workflow_count`
- [ ] `start_delegated_goal` (3) → `start_delegated_workflow` (arg `goal_handoff` KEEP)
- [ ] `close_goal` (4) / `create_goal` (2) → `close_workflow` / `create_workflow` (the `goal=` objective kwarg KEEP)
- [ ] `restore_running_after_failed_goal_start` (2) / `mark_waiting_goal` (2) → `restore_running_after_failed_workflow_start` / `mark_waiting_workflow`
- [ ] `delegated_goal_id` (3, arg name) → `delegated_workflow_id` (emitted payload key `goal_id` → Phase D)
- [ ] `request_goal_after_edit` (3, rule name) → `request_workflow_after_edit`
- [ ] `nested_goal` (10) / `initial_goal` (4) / `nested_goal_failure` (4) → `nested_workflow` / `initial_workflow` / `nested_workflow_failure` (scenario registry names)
- [ ] recorder symbols `_serialize_goal` (2) / `_handle_goal` (3) / `_ensure_goal_dir` (2) / `_goal_dir` (4) / `goal_dir` (10) / `goal_dirs` (6) / `_goal_seq_counter` (3) → `_serialize_workflow` / `_handle_workflow` / `_ensure_workflow_dir` / `_workflow_dir` / `workflow_dir` / `workflow_dirs` / `_workflow_seq_counter` (on-disk `goal.json`/`goal_<NN>` artifact names → Phase D)
- [ ] EventType members + values `GOAL_STARTED`/`GOAL_COMPLETED`/`GOAL_REQUESTED`/`RECURSIVE_GOAL_REQUESTED`/`RECURSIVE_GOAL_COMPLETED` → `WORKFLOW_* = "workflow_*"` / `RECURSIVE_WORKFLOW_* = "recursive_workflow_*"` (NAME **and** value edited whole — verified no persisted/string-match consumer)
- [ ] `WAITING_GOAL` member **name** → `WAITING_WORKFLOW` (its value `"waiting_goal"` → Phase D)

### Phase D — RENAME → `workflow` (mandatory second gated commit: persisted / on-disk / cross-process bytes)

- [ ] `goals` DB table (`__tablename__`) + `ForeignKey("goals.id")` → `workflows` / `ForeignKey("workflows.id")`
- [ ] `goal_id` (147) — DB column on `iterations` + serialized key + `ScopeField` Literal + `require_field("goal_id")` + `ContextRefs`/`ContextPacket` + `AuditNode`/`NodeId` + `AgentLaunch.goal_id` + store params → `workflow_id` (atomic)
- [ ] `task_center_goal_id` (13) → `task_center_workflow_id` (rides the `goal_id` atomic rename; persisted to `sandbox_events.jsonl`)
- [ ] `uq_iteration_goal_sequence` (1) → `uq_iteration_workflow_sequence` (or accept cosmetic constraint-name drift per §3(a))
- [ ] in-memory `graph_summary["goals"]` RunReport key → `"workflows"` — **NOTE: Phase A** (verified not on-disk)
- [ ] on-disk capacity/full-stack summary `goals` key + `recursive_goals` (3) → `workflows` / `recursive_workflows`
- [ ] `waiting_goal` (4, persisted task-status **value**) → `waiting_workflow`
- [ ] `goal_start` (2, `submission_kind` value) → `workflow_start`
- [ ] `goal_closure_report` (3, payload key + `submission_kind` value) → `workflow_closure_report`
- [ ] `request_recursive_goal:` (16, action string, startswith match) → `request_recursive_workflow:`
- [ ] `goal.json` filename + `goal_<NN>_<id>` audit dir name (on-disk artifacts) → `workflow.json` / `workflow_<NN>_<id>`

### KEEP — stays `goal` (the objective the workflow pursues, + D2 planner tokens)

- [ ] `goal` (420, the objective field/text, `<goal>` tag, `goal_statement` block) → KEEP *(see nuance below — entity-binding local vars rename)*
- [ ] `deferred_goal_for_next_iteration` (90) + `deferred_goal` column (16) + `set_deferred_goal_for_next_iteration` (3) + `assert_predecessor_has_deferred_goal_for_next_iteration` (4) → KEEP (the deferred objective)
- [ ] `submit_plan_closes_goal` (125) / `submit_plan_defers_goal` (60) + `SubmitPlanClosesGoalInput`/`SubmitPlanDefersGoalInput` + `get_submit_plan_*_goal_description` (3 each) → **KEEP (D2)**
- [ ] `iteration_goal` (23, `<iteration_goal>` tag/param) → KEEP (per-iteration objective)
- [ ] `goal_handoff` (12) + `_validate_goal_handoff` → KEEP (the objective handed off)
- [ ] `recursive_handoff_goal` (9) / `goal_text` (3) / `goal_statement` (2) → KEEP (return/hold objective text)
- [ ] `goal_iteration_blocks` (4) / `_goal_statement_block` (2) / `_current_iteration_goal_child` (2) → KEEP (emit/label objective text)
- [ ] `PlannerDefersWithoutDeferredGoal` (9) / `defers_without_deferred_goal` (7) / `_defers_without_goal` (2) → **KEEP (D2)** — the deferred-objective scenario
- [ ] `closes_goal_terminal` (3, check key) → **KEEP (D2)** — checks the kept `submit_plan_closes_goal` tool
- [ ] `PLANNER_COMPLETES_GOAL_PLAN` / `PLANNER_DEFERS_GOAL_PLAN` EventType members + values (`planner_full_plan`/`planner_partial_plan`) → **KEEP (D2)** — planner *plan* events
- [ ] `context.goal_entry_minimal` (1, capacity-pack name) → **KEEP (D2)**
- [ ] `requested_by_task_id` → KEEP (entity field, no `goal` word — KEEP-by-spelling)
- [ ] `Goal.goal` text DB column + `goal=` objective kwargs → KEEP (the workflow's objective)

### The one nuance (not a blanket keep)

- [ ] Bare lowercase **`goal` (420)** and **`goals` (46)** are **context-split**: where they are the objective field/tag/text they STAY; where a local variable binds the unit (`goal = goal_store.get(...)` → `workflow = workflow_store.get(...)`) or a key names the unit collection they RENAME. Resolved per-occurrence by the discriminator — this is why the §5 acceptance gate uses an enumerated allowlist + proof-of-KEEP greps, never `grep goal → 0`.

---

## 6. RALPLAN-DR summary

**Mode:** MEDIUM (locked-scope semantic rename; two registers; mandatory two-commit
effort — symbol sweep + persisted-byte migration).

**Principles (3–5):**
3. **Byte-change partition (not phase-by-convenience):** a token is Phase D ONLY if
   renaming it changes a persisted/on-disk/cross-process byte (DB schema, serialized
   VALUE string, on-disk artifact). Everything else — including in-repo string-matches,
   ENTITY EventType members (NAME + value), in-memory `RunReport` keys, and in-repo
   closure-report asserts — is Phase A, validated by the Phase A pytest sweep (one miss
   fails an assert before commit). This shrinks Phase D to the irreducibly out-of-process
   surface.
4. **Behavior preservation is the bar:** same tests pass. The enumerated grep
   allowlists (`goal_store → 0`, `task_center.goal → 0`, the PascalCase axis set,
   the renamed snake_case set) prove completeness for the **production axis symbols +
   scenario classes** only — they are case-sensitive and word-bounded, so they do NOT
   catch test-local renames like `_FakeGoal` (no word boundary before `Goal`) or
   uppercase test consts (`PLANNER_COMPLETES_GOAL`, `RECURSIVE_GOAL` — `GOAL` ≠ `Goal`).
   Those test-local renames rest on the green pytest sweep, not the grep gates. NOT a
   blanket `Goal` grep (unsatisfiable — KEEPs survive). The Phase A allowlist gate is
   satisfiable because A1(4) pulls `.py` entity-prose into Phase A.
5. **One contiguous RED→green sweep** for Phase A (no green intermediate exists
   mid-rename); dirty-worktree-tolerant; stage explicit paths only; LSP-rename for
   PascalCase symbols (does not disturb KEEP strings — exactly the safety property).
   Phase D is a MANDATORY second commit in the same effort (D1).

**Top-3 decision drivers:**
1. **Two-register correctness** — the objective `goal` (`<goal>`, `Goal.goal`,
   `goal_handoff`, `deferred_goal_for_next_iteration`) and the D2 planner-facing
   tokens must survive; only the unit's identity becomes workflow. The whole task
   fails if these are conflated.
2. **Byte-change blast radius** — the irreducibly persisted surface (`goals` table,
   `goal_id`/`task_center_goal_id`, `waiting_goal`/`goal_start`/`goal_closure_report`
   VALUES, on-disk `goal.json` + capacity-summary artifacts) is a DB + wire + on-disk
   migration. Isolating exactly those into the mandatory Phase D commit contains the
   silent-break risk while keeping commit 1 verifiable.
3. **User intent fidelity** — the durable UNIT is Workflow everywhere it is an
   identity/lifecycle/store; "goal" survives exactly where the workflow's objective
   is meant.

**Options (≥2, pros/cons):**
- **Option 1: split by serialized=true/false** (v1-style: all `serialized=true` tokens
  in Phase D). Pros: simple rule. Cons: over-stuffs Phase D — it would push in-repo-only
  string-matches, ENTITY EventType member names, in-memory `graph_summary["goals"]`, and
  in-repo closure-report asserts into the high-risk DB commit even though renaming them
  changes no persisted byte and they are validated by the Phase A pytest run. Larger,
  riskier commit 2; rejected.
- **Option 2C (CHOSEN — the byte-change re-partition):** Phase A absorbs every IN-REPO-ONLY
  serialized token into its pytest-gated sweep (in-repo mock string-matches, ENTITY
  EventType members — NAME *and* value, verified to have no persisted/string-match
  consumer — in-repo closure-report asserts that don't touch persisted strings, in-repo
  check keys, the in-memory `graph_summary["goals"]` key); Phase D shrinks to exactly
  the out-of-process surface: DB DDL (`goals` table, `goal_id` column/FK/constraint),
  the PERSISTED status/audit VALUE strings (`waiting_goal`, `goal_start`,
  `goal_closure_report` payload VALUE, `task_center_goal_id`, `goal_id` jsonl key), and
  on-disk artifacts (`goal.json`, `sandbox_events.jsonl`, capacity/full-stack summary
  JSON). Pros: smallest possible commit 2; each Phase-A
  token is asserted by a test before commit 1; matches the project's surgical rules.
  Cons: requires per-token byte-change judgment (mitigated by the discriminator in §1
  and the per-row phase column in §2C). **This is the middle-ground partition and the
  chosen one.**
- **Option 3: do it all at once (symbols + DB + all serialized values in one commit).**
  Pros: no transient seam. Cons: enormous single diff spanning DB migration + sandbox
  audit wire + jsonl + mock string-matches; one missed VALUE string silently breaks the
  mock/audit with no import error; high collision probability with parallel agents; hard
  to review/revert. Rejected — too risky for one atomic change.
- **Option 4: KEEP serialized entity tokens as `goal` (v1's stance).** Pros: zero
  migration. Cons: **violates the locked target model** (D1) — entity tokens must
  become workflow, and Phase D is mandatory. Rejected.

**Note on the transient seam:** between commit 1 and commit 2 the *symbol* is
`Workflow` while the *persisted column/value/key* is still `goal_id`/`waiting_goal`.
Per D1 this seam is bounded — Phase D is a required same-effort second commit, not an
indefinite state.

**Risks & mitigations:**

| Risk | Likelihood | Mitigation |
|---|---|---|
| Conflating the two registers — renaming `<goal>`/`Goal.goal`/`goal_handoff`/D2 tokens | Medium | Decisions table 2B + per-sentence prose rule + proof-of-KEEP + D2-KEEP greps assert objective/planner tokens still present after Phase A. |
| Silent contract break — renaming a persisted VALUE string with no import error | Medium-High | All persisted-byte tokens isolated into Phase D with producer+consumer lockstep lists; `test_correctness.py` + sandbox audit/contract tests exercise the matched paths; Phase A explicitly KEEPs the VALUE bytes (member NAMES rename safely — verified VALUEs not in jsonl). |
| Phase A allowlist gate unsatisfiable (bare-`Goal` prose lingers) | Was High | A1(4) pulls `.py` entity-prose into Phase A; only `docs/*.html`+`CLAUDE.md` (outside gate scope) remain → gate satisfiable. |
| Phase D DDL creates empty `workflows` + never migrates `goals` rows (state split) | Medium-High | `_rename_tables` runs PRE-`create_all` (between lines 302–304) preserving rows; legacy-check raises only when both tables coexist; fresh-DB + round-trip tests gate. |
| Missed monkeypatch target (`_nested_workflow_depth_gt_1`, 5 sites) | Medium | Listed as lockstep consumer in A3; test fails loudly if stale. |
| Parallel-agent dirty worktree collision | Medium | One fast Phase-A sweep; explicit-path staging; verify at HEAD before commit; do not touch unrelated dirty `probe_bridge.py`/`mock_event_source`. |

---

## 7. Resolved decisions (LOCKED — no sign-off pending)

All prior open decisions are resolved by D1, D2, and the adopted defaults. Recorded
here for traceability:

1. **Planner-facing tokens** (`submit_plan_closes_goal`/`submit_plan_defers_goal`,
   `PLANNER_*_GOAL_PLAN` member+value, `PlannerDefersWithoutDeferredGoal`,
   `context.goal_entry_minimal`, `closes_goal_terminal`): **RESOLVED — KEEP `goal`**
   (D2, objective reading). No rename, no Phase D.
2. **Phase D timing:** **RESOLVED — mandatory second gated commit in the same effort**
   (D1). Not optional, not deferrable; the effort completes only when commit 2 lands.
3. **`goal_id` → `workflow_id` scope:** **RESOLVED — single atomic rename in Phase D**
   across DB column + FK + `uq_iteration_goal_sequence` + `ScopeField` + `ContextPacket`
   + `AuditNode`/`NodeId` + `task_center_goal_id` + on-disk jsonl + mock reads + tests.
4. **`ContextEngineDeps.goal_store` → `workflow_store`:** **RESOLVED — rename in Phase A**
   (in-process DI attribute; gated by the §5 `goal_store → 0` grep).
5. **File renames `db/models/goal.py` / `db/stores/goal_store.py`:** **RESOLVED —
   rename to `workflow.py` / `workflow_store.py` in Phase A** (`git mv`; module-path
   strings in `_EXPORTS` move with it).
