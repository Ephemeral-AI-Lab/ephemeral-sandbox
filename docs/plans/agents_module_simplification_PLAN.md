# Agents Module Simplification Plan

Scoped follow-up to `class_field_simplification_PLAN.md`, covering the
`backend/src/agents/` definition model only. Source of truth:
`backend/src/agents/definition/model.py` (`AgentDefinition`). Three field/enum
removals, ordered by blast radius. ① and ② are genuine low-risk redundancies;
③ is a design task, not a deletion.

Evidence base: code anchors verified 2026-05-30 against `main`.

## Progress

- **① — DONE** (2026-05-30). Field + Protocol + config knob removed; factory
  builds rules from triggers + defaults.
- **② — DONE** (2026-05-30). Field removed, gate collapsed, MDs cleaned.
- **③ — not started.** Design task; see section below.

Verification: `test_agents/`, `test_tools/test_submission_terminal_routing.py`,
`test_engine/`, `test_task_center/` green (583 passed). All 6 real profiles load
under `extra="forbid"`. One pre-existing failure
(`test_attempt_launcher_retry.py::test_attempt_harness_records_runner_token_usage`,
`EphemeralRunResult ... event_count`) belongs to the parallel mock event-source
migration — not this change.

---

## ① Collapse `notification_rules` → keep `notification_triggers`  (clean win)  ✅ DONE

**Finding.** The two fields are not duplicates of each other:
`notification_triggers: list[str]` is declarative frontmatter IDs;
`notification_rules: list[AgentNotificationRule]` is resolved rule *objects*.
But in the loaded path `notification_rules` is **dead**:

- No `.md` can populate it — the Protocol fields are `Callable`s,
  unexpressible in YAML. Every profile uses `notification_triggers:`.
- No production code constructs `AgentDefinition(notification_rules=...)`
  (grep empty outside tests).
- `factory.py:380-385` merges `list(agent_def.notification_rules)` (always
  `[]`) + `resolve_harness_notification_triggers(triggers)` + defaults into one
  list.

**Change.**
- Drop the `notification_rules` field from `AgentDefinition`.
- `factory.py` builds rules purely from `triggers + defaults`.
- **Cascade:** also delete the `AgentNotificationRule` Protocol and the
  `arbitrary_types_allowed=True` knob in `model_config` (its only reason to
  exist).

**Cost / surface.** Only test fixtures that inject rule objects directly need
migrating to `notification_triggers`.

**Verify.** `agents` unit tests + `engine/agent/factory` tests green; a loaded
profile still gets default + trigger-resolved rules at launch.

---

## ② Remove `dispatchable_by_planner`  (pure redundancy)  ✅ DONE

**Finding.** Single consumer — `_is_generator_capable_agent`
(`tools/submission/planner/_schemas.py:102`):

```python
return definition.dispatchable_by_planner and definition.agent_kind in {EXECUTOR, VERIFIER}
```

Frontmatter proves the flag adds nothing: exactly `executor.md` and
`generator_verifier.md` set `dispatchable_by_planner: true`, and those are
exactly the `{EXECUTOR, VERIFIER}` kinds. The two clauses select the same set.

**Change.**
- Remove the field from `AgentDefinition`.
- Collapse the gate to `definition.agent_kind in {EXECUTOR, VERIFIER}`.
- Drop `dispatchable_by_planner: true` from the two profile MDs.

**Coupling with ③.** This collapse depends on `agent_kind` existing. Do ②
**before** ③ (or fold the gate replacement into ③'s design).

**Verify.** Planner submission tests: an executor/verifier name passes the gate,
a planner/advisor/explorer name is rejected.

---

## ③ Decouple terminal routing + retag `agent_kind`  (design — DRAFT)

> **Decision needed (read first).** The request is "remove `agent_kind`." The
> honest answer from the code: the **routing coupling** can be removed cleanly,
> but the **tag cannot** — three non-routing consumers depend on it (telemetry
> `metadata["role"]`, mock-runner dispatch, planner gate). So ③ is two separable
> changes: **A — decouple the router** (the real, clean win), and **B — what
> replaces the tag** (the only actual fork). This draft recommends **A + rename
> the enum `AgentKind` → `AgentRole`** (keep it typed, strip its routing role),
> and rejects full deletion. You can override B before we implement.

### Consumer map (verified 2026-05-30)

| consumer | use | change |
|---|---|---|
| `task_center/_core/terminal_tool_routing.py:127-150` (`_allowed_terminals`) | depth-aware terminal filtering via `if agent_kind == PLANNER … elif EXECUTOR …` | **A** — replaced by per-folder routing |
| `engine/agent/factory.py:378` + `tools/subagent/run_subagent/run_subagent.py:198` + mock `runner.py:311,1762,2012`, `scenario_loop_runner.py:167,213` | `metadata["role"] = agent_kind.value` (telemetry tag) | **B** — read `.role` instead |
| mock `runner.py:253-282` (invocation event + `_run_<role>`), `scenario_adapter.py:285` (`TurnScript` select) | behavioral dispatch on `agent_kind.value == "planner"/...` | **B** — dispatch on `.role` |
| `tools/submission/planner/_schemas.py:102` | generator gate `agent_kind in {EXECUTOR, VERIFIER}` (post-②) | **B** — `role in {EXECUTOR, VERIFIER}` |

The enum itself carries **no routing logic** — it is a plain `StrEnum`. All
routing coupling lives in the `terminal_tool_routing.py` ladder. A and B are
therefore independent and can land in either order.

---

### Change A — decouple the router (the substantive win)

**Scope is small: only 2 of 6 profiles have routing rules** (planner, executor).
Each rule is a pure function `(depth: int, has_workflow: bool) -> frozenset[str] | None`:

- **planner:** `depth>1 → {submit_plan_closes_goal}`; else `{closes, defers}`.
- **executor:** `has_workflow=False → None`; `depth>1 → {success, blocker}`;
  else `{handoff, success, blocker}`.
- **verifier / evaluator / advisor / explorer:** no file → `None` (no filtering).

**Mechanism (recommended): optional sibling routing module per profile.**
The user asked for "script-based, in the agent's own folder." Minimal way to
honor that without a plugin framework: a convention-discovered optional sibling.

- Add `routing.py` next to a profile `.md` that needs filtering, exporting one
  function `select_terminals(depth, has_workflow) -> frozenset[str] | None`.
  Only `profile/main/planner.py`(routing) and `profile/main/executor.py`(routing)
  exist; the other four ship none.
- Loader change (`agents/definition/loader.py`): after building each
  `AgentDefinition`, look for a sibling module by convention (e.g.
  `<stem>_routing.py`) and, if present, import it via `importlib.util` and attach
  the callable to a new optional field `terminal_router: Callable | None = None`
  (excluded from serialization; `arbitrary_types_allowed` is NOT reintroduced —
  use `model_config`-free attach or a `PrivateAttr`).
- `TerminalToolRouter._allowed_terminals` collapses to: if
  `definition.terminal_router is None: return None`; else
  `return definition.terminal_router(depth=_depth(ctx), has_workflow=ctx.scope.workflow_id is not None)`.
  The enum branch, the `AgentKind` import, and the hardcoded terminal sets all
  leave `terminal_tool_routing.py`.

**Proportionality note / alternative.** Two functions do not justify dynamic
module loading on their own. A lighter alternative is a **declarative routing
block in frontmatter** (depth/workflow conditions → terminal lists) interpreted
by the router — no code import, no new loader capability. It is less
"script-based" than the user asked for; offered as the cheaper option if the
import mechanism is judged too heavy for two profiles.

**Test seam.** `test_terminal_tool_router.py` monkeypatches
`_nested_workflow_depth_gt_1` by module path. The router still calls that helper
and passes its boolean result into the per-folder function (as `is_nested`), so
the seam is unchanged. Per-folder functions receive `is_nested` / `has_workflow`
as arguments — they never touch `ContextScope` or compute depth — which keeps
them trivially unit-testable in isolation.

#### Concrete sketch

`backend/src/agents/profile/main/planner_routing.py`:
```python
"""Launch-time terminal routing for the planner profile."""
from __future__ import annotations


def select_terminals(*, is_nested: bool, has_workflow: bool) -> frozenset[str]:
    # A nested planner (caller attempt is itself inside a workflow) may only
    # close its goal; a top-level planner may also defer.
    if is_nested:
        return frozenset({"submit_plan_closes_goal"})
    return frozenset({"submit_plan_closes_goal", "submit_plan_defers_goal"})
```

`backend/src/agents/profile/main/executor_routing.py`:
```python
"""Launch-time terminal routing for the executor profile."""
from __future__ import annotations


def select_terminals(*, is_nested: bool, has_workflow: bool) -> frozenset[str] | None:
    # Outside a workflow: keep the full frontmatter terminal set (no filtering).
    if not has_workflow:
        return None
    # Nested executors cannot hand off; only succeed or block.
    if is_nested:
        return frozenset({"submit_execution_success", "submit_execution_blocker"})
    return frozenset(
        {
            "submit_execution_handoff",
            "submit_execution_success",
            "submit_execution_blocker",
        }
    )
```

The other four profiles ship no `*_routing.py`, so the loader attaches no router
and they are never filtered (today's `return None` for non-planner/executor).

`terminal_tool_routing.py` — the enum ladder collapses to a thin dispatch:
```python
@staticmethod
def _allowed_terminals(definition, ctx) -> frozenset[str] | None:
    router = definition.terminal_router
    if router is None:
        return None
    return router(
        is_nested=_nested_workflow_depth_gt_1(ctx),
        has_workflow=ctx.scope.workflow_id is not None,
    )
```
The `AgentKind` import, the `{PLANNER, EXECUTOR}` membership test, and the four
hardcoded terminal sets all leave this file.

`agents/definition/model.py` — carry the callable as a private attribute (no
`arbitrary_types_allowed`, not serialized; survives `model_copy`):
```python
from pydantic import PrivateAttr

_terminal_router: Callable[..., frozenset[str] | None] | None = PrivateAttr(default=None)

@property
def terminal_router(self) -> Callable[..., frozenset[str] | None] | None:
    return self._terminal_router
```

`agents/definition/loader.py` — convention discovery after `model_validate`:
```python
routing_path = path.with_name(f"{path.stem}_routing.py")
if routing_path.is_file():
    spec = importlib.util.spec_from_file_location(
        f"agents._routing.{path.stem}", routing_path
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    definition._terminal_router = mod.select_terminals
```

**Threshold note.** Passing the `is_nested` boolean (not raw `depth: int`) fixes
the ">1" threshold in the router, matching today's behavior exactly and keeping
the monkeypatch seam. If a future profile needs its own threshold, switch the
contract to pass `depth: int` and move the test seam from
`_nested_workflow_depth_gt_1` to `_depth`. Not needed for the two current rules.

---

### Change B — retag `agent_kind` → `AgentRole`  (recommended)

`agent_kind` survives as pure identity once routing is gone. Rename to signal
that:

- `AgentKind` enum → `AgentRole` (members unchanged: planner / executor /
  verifier / evaluator / advisor / explorer).
- `AgentDefinition.agent_kind: AgentKind` → `role: AgentRole`. Frontmatter key
  `agent_kind:` → `role:` in all 6 MDs; loader's required-field check updated.
- Mechanical rename at the four B-consumers above (`.agent_kind` → `.role`,
  `agent_kind.value` → `.role.value`).

**Why keep it typed (reject full deletion).** Full deletion is *technically*
possible today because `name == kind` for all six profiles, so every consumer
could collapse to `agent_def.name`. **Rejected** because:
- It conflates instance identity (`name`) with role category — and breaks the
  first time the framework has two profiles of one role (e.g. `executor` +
  `executor_v2`), which the multi-agent direction explicitly anticipates
  (see memory: "typed roles").
- A free-form `str` tag reintroduces typo-silent failure in the planner gate and
  mock dispatch that the enum prevents.

Keeping a typed `AgentRole` preserves exhaustiveness and the closed member set
while still removing `agent_kind`'s routing responsibility — which is the real
intent of ③.

---

### Migration order (within ③)

1. **A first** — add `terminal_router` field + loader discovery + the two
   `routing.py` files; collapse `_allowed_terminals`. `agent_kind` still exists
   but the router no longer reads it. Verify `test_terminal_tool_router.py` +
   `test_submission_terminal_routing.py` green.
2. **B second** — rename enum + field + frontmatter + the four consumers.
   Verify `test_agents/`, mock-runner suites, planner-gate tests green.

### Risks / open items

- **Mock-runner dispatch is the largest surface** (~7 sites across 3 files); it
  is a pure rename under B, but touch-count is high — do it mechanically.
- Decide the per-folder mechanism (sibling module vs declarative block) before
  starting A.
- Confirm `terminal_router` can be attached without re-enabling
  `arbitrary_types_allowed` (use `PrivateAttr` or post-validate attach).

---

## Sequencing

1. **① — DONE.**
2. **② — DONE.**
3. **③ — drafted above.** Resolve the two decisions (tag = rename vs delete;
   routing = sibling-module vs declarative), then implement A → B.
