# Agent-Loop Termination Refactor — FINAL Plan

**Status:** FINAL (ralplan consensus iteration 4, APPROVE)
**Scope:** Replace the dual-counter / multi-rule / multi-exit-reason termination subsystem of the EphemeralOS agent query loop with a minimal single-failure-mode design.

---

## Operating principle (locked)

> **The agent loop keeps looping until a valid terminal tool is submitted. The only failure condition is reaching `ceil(1.5 × tool_call_limit)` tool calls without a terminal submission.**

Two global invariants enforce this minimal contract:

1. **Every agent declares at least one terminal tool.** `AgentDefinition.terminals` is non-empty; the derived `terminal_tools` set on `QueryContext` is non-empty by construction.
2. **Every agent declares a tool-call budget.** `AgentDefinition.tool_call_limit: int` is required (no `None`, no default). The `QueryContext.tool_call_limit` field has type `int` (not `int | None`).

These invariants are enforced at config-load time (Pydantic validators) AND at agent-spawn time (assertions after registry derivation). Any profile that omits either field fails loud at boot.

---

## Final design

### Exit reasons — TWO

```python
class QueryExitReason(StrEnum):
    TOOL_STOP = "tool_stop"                              # success: terminal tool submitted
    TERMINAL_NOT_SUBMITTED = "terminal_not_submitted"    # failure: hard ceiling crossed
```

`TEXT_RESPONSE`, `RESOURCE_LIMIT`, `TERMINAL_REFUSED` are deleted.

### Failure predicate — ONE LINE

```python
def terminal_submission_failed(context: QueryContext) -> bool:
    """True iff the agent has burned 1.5× its tool_call_limit without a terminal submission."""
    return context.tool_calls_used >= math.ceil(1.5 * context.tool_call_limit)
```

No `None` check (invariant 2 guarantees `tool_call_limit` is an int). No `terminal_tools` gate (invariant 1 guarantees it's non-empty). Lives module-level in `backend/src/engine/query/loop.py`.

### Exit decision — 5 lines in `_run_query_loop`

```python
if context.terminal_result is not None:
    context.exit_reason = QueryExitReason.TOOL_STOP
    break
if terminal_submission_failed(context):
    if background_tasks is not None:
        await background_tasks.cancel_all()
    context.exit_reason = QueryExitReason.TERMINAL_NOT_SUBMITTED
    break
# else: continue. terminal_call_reminder will fire next turn.
```

That's the whole exit logic. Nothing else.

### Notification rule — ONE

```python
def make_terminal_call_reminder() -> NotificationRule:
    """Nudge the agent to submit a terminal tool.

    Fires every turn after the first assistant message while terminal_result
    is None. No tier gating, no dedup state — the reminder repeats until the
    agent complies or the hard ceiling kills the run.
    """
    def _trigger(messages, context):
        return (
            context.terminal_result is None
            and any(m.role == "assistant" for m in messages)
        )

    def _body(messages, context):
        names = ", ".join(sorted(context.terminal_tools))
        used = context.tool_calls_used
        limit = context.tool_call_limit
        ceiling = math.ceil(1.5 * limit)
        turns_remaining = max(0, ceiling - used)
        return (
            f"You have not submitted a terminal tool. Deliver your result "
            f"by calling one of: {names}. Budget: {used}/{limit} tool calls "
            f"used; the run will fail at {ceiling} tool calls "
            f"({turns_remaining} remaining)."
        )

    return NotificationRule(
        name="terminal_call_reminder",
        body=_body,
        trigger=_trigger,
        fire_once=False,
    )
```

Replaces `make_budget_warning`, `make_budget_overflow_reminder`, AND `make_missing_terminal_reminder` (all three deleted). The `turns_remaining` figure provides an urgency gradient without needing separate rules or tier state.

### Loop pseudocode (full body of `_run_query_loop`)

```python
async def _run_query_loop(context, messages):
    background_tasks, notification_service = _prepare_query_loop_runtime(context)
    try:
        while True:
            executor = await _build_stream_executor(context, background_tasks, messages)

            if context.notification_rules:
                await dispatch_rules(
                    context.notification_rules, messages, context, notification_service,
                )
                pending = notification_service.pop_pending_notifications()
                if pending:
                    messages.append(Message(role="user", content=list(pending)))

            state = _ProviderStreamAccumulator()
            run_request = build_query_run_request(context, messages)
            async for event, usage in _consume_provider_stream(
                context, executor, run_request, state,
            ):
                yield event, usage
            for progress in executor.get_progress():
                yield progress, None
            for emitted in executor.get_events():
                yield emitted, None

            final_message = state.final_message
            messages.append(final_message)
            run_request.prompt_report.record_assistant(
                seq=run_request.prompt_report_seq,
                message=final_message,
                usage=state.usage,
            )
            yield AssistantMessageCompleteEvent(
                message=final_message, usage=state.usage,
                agent_name=context.agent_name, run_id=context.run_id,
            ), state.usage

            # Dispatch tools if present, capture terminal_result, append tool_results.
            if final_message.tool_uses:
                dispatch = await dispatch_assistant_tools(
                    context, messages, final_message, executor,
                    streamed_tool_use_ids=state.streamed_tool_use_ids,
                    background_tasks=background_tasks,
                )
                for event in dispatch.events:
                    yield event, None
                tool_results = list(dispatch.tool_results)
                run_request.prompt_report.record_tool_results(
                    seq=run_request.prompt_report_seq,
                    tool_results=tool_results,
                )
                for event in flush_system_notification_events(notification_service):
                    yield event, None
                if dispatch.terminal_result is not None:
                    context.terminal_result = dispatch.terminal_result
                if tool_results:
                    messages.append(Message(role="user", content=list(tool_results)))

            # Single exit-decision block.
            if context.terminal_result is not None:
                context.exit_reason = QueryExitReason.TOOL_STOP
                break
            if terminal_submission_failed(context):
                if background_tasks is not None:
                    await background_tasks.cancel_all()
                yield (ToolExecutionCompletedEvent(
                    tool_name="",
                    output=_terminal_not_submitted_message(context),
                    is_error=True,
                ), None)
                for event in flush_system_notification_events(notification_service):
                    yield event, None
                context.exit_reason = QueryExitReason.TERMINAL_NOT_SUBMITTED
                break
            # Otherwise: loop. terminal_call_reminder fires next iteration.
    finally:
        if background_tasks is not None and background_tasks.has_pending():
            await background_tasks.cancel_all()
```

`_dispatch_final_message_tools` is deleted; its body is inlined here.

### Synthetic event helper

```python
def _terminal_not_submitted_message(context: QueryContext) -> str:
    return (
        f"Agent stopped: terminal tool not submitted. "
        f"tool_calls_used={context.tool_calls_used}, "
        f"tool_call_limit={context.tool_call_limit}, "
        f"hard_ceiling={math.ceil(1.5 * context.tool_call_limit)}."
    )
```

---

## State on `QueryContext`

**Kept (unchanged):** `api_client`, `tool_registry`, `cwd`, `model`, `system_prompt`, `max_tokens`, `agent_name`, `run_id`, `task_center_task_id`, `tool_calls_used: int = 0`, `tool_metadata`, `enable_background_tasks`, `exit_reason`, `terminal_result`, `prompt_report_recorder`, `notification_rules`, `notification_fired`, `notification_state`.

**Type tightened:**
- `tool_call_limit: int` (was `int | None = None`) — required, integer.
- `terminal_tools: set[str]` — same type, but non-empty by invariant.

**Deleted:**
- `max_tolerance_after_max_tool_call: int | None`
- `text_only_no_terminal_turns: int`
- `tool_overshoot` property
- `overshoot_units` property
- `tool_budget` property + `_ToolBudgetView` dataclass

---

## Invariant enforcement

### `AgentDefinition` (Pydantic, config-load time)

```python
tool_call_limit: int = Field(..., gt=0)  # required, positive
terminals: list[str] = Field(..., min_length=1)  # required, non-empty

@field_validator("terminals")
@classmethod
def _check_terminals(cls, terminals: list[str]) -> list[str]:
    cleaned = [t for t in terminals if t.strip()]
    if not cleaned:
        raise ValueError("AgentDefinition.terminals must be non-empty")
    return cleaned
```

Pydantic's `extra="forbid"` (already set) rejects legacy YAMLs carrying the deleted `max_tolerance_after_max_tool_call` key.

### `_finalize_tool_registry_and_prompt` (spawn-time, registry-derived)

```python
terminal_tool_names = [
    t.name for t in tool_registry.list_tools()
    if getattr(t, "is_terminal_tool", False)
]
assert terminal_tool_names, (
    f"Agent {agent_def.name!r} has no terminal-capable tool registered. "
    f"Every agent must declare at least one tool with is_terminal_tool=True."
)
```

Catches the case where a profile declares `terminals:` but the named tools aren't registered.

---

## Files touched (single commit)

### Source

- **`backend/src/engine/query/context.py`** — `QueryExitReason`: drop 3 variants, keep 2. `QueryContext`: drop 2 fields and 2 properties, tighten `tool_call_limit` type. Delete `_ToolBudgetView`.
- **`backend/src/engine/query/loop.py`** — Rewrite `_run_query_loop` body per pseudocode above. Add `terminal_submission_failed` and `_terminal_not_submitted_message` helpers. Delete `_dispatch_final_message_tools`. `import math`.
- **`backend/src/notification/rules/factories.py`** — Delete `make_budget_warning`, `make_budget_overflow_reminder`, `make_missing_terminal_reminder` and their module-level state keys. Add `make_terminal_call_reminder`. Keep `make_opening_reminder` untouched.
- **`backend/src/notification/rules/__init__.py`** and **`backend/src/notification/__init__.py`** — Update exports: drop three rule factories, add one.
- **`backend/src/agents/definition/model.py`** — Delete `max_tolerance_after_max_tool_call` field + `_coerce_nonneg_int` validator. Tighten `tool_call_limit` to required `int = Field(..., gt=0)`. Tighten `terminals` to `min_length=1` with non-empty validator.
- **`backend/src/engine/agent/factory.py`** — Rename `_attach_default_overshoot_rules` → `_attach_default_terminal_reminder`; body appends only `make_terminal_call_reminder()`. Drop `max_tolerance` plumbing. Add `assert terminal_tool_names` in `_finalize_tool_registry_and_prompt`.
- **`backend/src/engine/agent/lifecycle.py`** — Rewrite module docstring; remove references to deleted exit reasons.
- **`backend/src/config/sections/engine.py`** — Delete `budget_overflow_reminder_every`. `EngineConfig` becomes an empty `ModuleConfigBase` subclass.
- **`backend/src/tools/_framework/execution/tool_call.py`** — Update `_count_tool_dispatch` docstring (drop refs to deleted symbols).

### Profile audit (pre-implementation)

Before the commit, sweep all agent profiles to confirm they satisfy the new invariants:

```bash
# Every profile MD must declare terminals.
grep -L "^terminals:" backend/src/agents/profile/**/*.md

# Every profile MD must declare tool_call_limit.
grep -L "^tool_call_limit:" backend/src/agents/profile/**/*.md
```

Any profile surfaced by either command must be retrofitted or removed before the refactor lands. Without this audit, agent spawning will fail at boot.

### Doc

- **`docs/architecture/agent_loops/main-loop.html`** — Replace `<section id="budget-and-terminal-refusal">` with the new minimal-design body (see below).

---

## Doc rewrite — `<section id="budget-and-terminal-refusal">`

```html
<section id="budget-and-terminal-refusal" data-last-reviewed-commit="HEAD"
         data-evidence-paths="backend/src/engine/query/loop.py
                              backend/src/engine/query/context.py
                              backend/src/notification/rules/factories.py
                              backend/src/engine/agent/factory.py">
  <h2>Termination</h2>
  <p>
    The agent loop has one job: keep looping until a terminal tool is submitted.
    The only failure condition is reaching <code>ceil(1.5 × tool_call_limit)</code>
    tool calls without a terminal submission.
  </p>
  <p>
    Two global invariants make this design minimal:
  </p>
  <ul>
    <li>Every agent declares at least one terminal tool
        (<code>AgentDefinition.terminals</code> non-empty).</li>
    <li>Every agent declares an integer tool-call budget
        (<code>AgentDefinition.tool_call_limit: int</code>).</li>
  </ul>
  <p>Exit reasons:</p>
  <ul>
    <li><code>TOOL_STOP</code> — a terminal-tool dispatch returned
        <code>is_terminal=True</code>. The terminal tool's
        <code>ToolResult</code> is the run's deliverable.</li>
    <li><code>TERMINAL_NOT_SUBMITTED</code> — the run hit
        <code>tool_calls_used &gt;= ceil(1.5 × tool_call_limit)</code>
        without a terminal submission. Background tasks are cancelled and
        a synthetic <code>ToolExecutionCompletedEvent(is_error=True)</code>
        is emitted on the stream.</li>
  </ul>
  <p>
    A single notification rule, <code>terminal_call_reminder</code>, fires
    every turn after the first assistant message while
    <code>terminal_result</code> is None. The reminder body includes the
    registered terminal tool names, the current <code>used/limit</code>
    budget, the hard ceiling, and the remaining turns until failure.
  </p>
  <p>
    Transcript invariant: on the <code>TERMINAL_NOT_SUBMITTED</code> exit,
    if the failing assistant turn produced <code>tool_use</code> blocks,
    the paired <code>tool_result</code> blocks are appended to the
    transcript before the loop breaks — preserving the Anthropic Messages
    API contract.
  </p>
</section>
```

---

## Test plan

### Delete

- `backend/tests/unit_test/test_engine/test_overshoot_accounting.py` — exercises deleted properties.
- `backend/tests/unit_test/test_notification/test_overshoot_rules.py` — exercises deleted rule factories.
- `make_budget_warning` test cases in `backend/tests/unit_test/test_notification/test_rules_factories.py`.
- `make_budget_warning` wiring at `backend/tests/unit_test/test_tools/test_tool_execution.py:754`.

### Rewrite (drive `_run_query_loop` via streaming-provider fake; repo has prior art)

- `backend/tests/unit_test/test_engine/test_soft_limit_behavior.py` → `test_hard_ceiling_behavior.py`. Cases:
  1. `tool_calls_used == ceiling - 1` → no exit.
  2. `tool_calls_used == ceiling` → exit `TERMINAL_NOT_SUBMITTED` with synthetic event.
  3. Terminal short-circuit beats hard ceiling (`terminal_result` set + `used >> ceiling` → `TOOL_STOP`).
  4. Background tasks cancelled on hard exit.
- `backend/tests/unit_test/test_engine/test_loop_resource_limit_transcript.py` → `test_terminal_not_submitted_transcript.py`. Same pairing invariant (assistant tool_use blocks paired with user tool_result blocks before the break), asserted against the new exit reason.

### Trim

- `backend/tests/unit_test/test_engine/test_tool_call_limit.py` — drop `_coerce_nonneg_int` validator cases. Update the `tool_call_limit` validator cases: it is now `required, gt=0`; assert that omission or non-positive values raise `ValidationError`.

### New

- `backend/tests/unit_test/test_notification/test_terminal_call_reminder.py` (~4 cases):
  1. Silent on the opening turn (no assistant message yet).
  2. Fires when an assistant message exists and `terminal_result is None`.
  3. Silent once `terminal_result` is set.
  4. Body contains `terminal_tools`, `used`, `limit`, ceiling, and `turns_remaining`.
- `backend/tests/unit_test/test_agents/test_definition_invariants.py`:
  1. `AgentDefinition(name="x", description="y", terminals=[], tool_call_limit=10)` raises `ValidationError`.
  2. `AgentDefinition(name="x", description="y", terminals=["submit_x"])` (no `tool_call_limit`) raises `ValidationError`.
  3. `AgentDefinition(..., max_tolerance_after_max_tool_call=5)` raises `ValidationError` (extra="forbid" guard, proves no silent shim).

### Keep verbatim

- `backend/tests/unit_test/test_engine/test_lifecycle.py::*` — `TOOL_STOP` assertions still valid.
- `TOOL_STOP` cases in `backend/tests/unit_test/test_tools/test_tool_execution.py`.
- `make_opening_reminder` tests in `test_rules_factories.py`.

---

## Acceptance criteria

### Mechanical (grep-zero)

```bash
grep -rn "max_tolerance_after_max_tool_call\|text_only_no_terminal_turns\|tool_overshoot\|overshoot_units\|_ToolBudgetView\|tool_budget\|RESOURCE_LIMIT\|TERMINAL_REFUSED\|TEXT_RESPONSE\|make_budget_warning\|make_budget_overflow_reminder\|make_missing_terminal_reminder\|budget_overflow_reminder_every\|_dispatch_final_message_tools\|_attach_default_overshoot_rules" backend/ docs/
# Expected: zero matches.
```

### Mechanical (grep-present)

```bash
grep -rn "TERMINAL_NOT_SUBMITTED\|terminal_submission_failed\|make_terminal_call_reminder\|_attach_default_terminal_reminder" backend/
# Expected: hits in context.py, loop.py, factories.py, factory.py, the two notification __init__.py, and the new/rewritten tests.
```

### Mechanical (profile audit, pre-commit)

```bash
grep -L "^terminals:" backend/src/agents/profile/**/*.md
grep -L "^tool_call_limit:" backend/src/agents/profile/**/*.md
# Expected: zero matches (every profile declares both).
```

### Pytest (must pass)

```bash
.venv/bin/pytest backend/tests/unit_test/test_engine/ -x -q
.venv/bin/pytest backend/tests/unit_test/test_notification/ -x -q
.venv/bin/pytest backend/tests/unit_test/test_tools/test_tool_execution.py -x -q
.venv/bin/pytest backend/tests/unit_test/test_agents/ -x -q
```

### Lint

```bash
.venv/bin/ruff check backend/src/engine backend/src/notification backend/src/config backend/src/agents
```

### Doc consistency (human review)

`docs/architecture/agent_loops/main-loop.html` mentions only `TOOL_STOP` and `TERMINAL_NOT_SUBMITTED`. No references to deleted symbols.

---

## Pre-mortem (3 scenarios)

### Scenario A — Pathological agent emits text forever without calling any tool
`tool_calls_used` never increments, so `terminal_submission_failed` stays false. The reminder fires every turn but the model ignores it. The loop runs until the caller cancels.

**Mitigation:** none at this layer. Documented as accepted tradeoff. Callers (`run_ephemeral_agent` consumers — `task_center/attempt/launch.py`, `tools/ask_helper/ask_advisor/ask_advisor.py`, `task_center_runner/benchmarks/sweevo/run.py`, `tools/subagent/run_subagent/run_subagent.py`) may wrap with their own deadlines if real traces demand it. Tracked as follow-up.

### Scenario B — Profile loaded with no terminal tools registered
`AgentDefinition` validator fires at config-load → `ValidationError` with a clear message. If somehow the registry-derivation path is hit (e.g., profile declares `terminals: [foo]` but `foo` is not registered), the spawn-time assertion in `_finalize_tool_registry_and_prompt` fires → `AssertionError` with the agent name.

**Mitigation:** both gates are in place; the failure mode is structurally impossible past boot.

### Scenario C — Legacy YAML still carrying `max_tolerance_after_max_tool_call: 10`
Pydantic `extra="forbid"` on `AgentDefinition` raises `ValidationError("Extra inputs are not permitted")` at config-load.

**Mitigation:** desired hard break — no silent shim. Migration runbook (one line: delete the key from your YAML) goes into the commit message.

---

## ADR

**Decision.** Reduce the agent-loop termination subsystem to: two exit reasons (`TOOL_STOP`, `TERMINAL_NOT_SUBMITTED`), one failure predicate (`terminal_submission_failed`), one notification rule (`make_terminal_call_reminder`), one 5-line exit decision block. Enforce global invariants that every agent has at least one terminal tool and a positive-integer `tool_call_limit`.

**Drivers.**
1. **Operating principle fidelity.** "Keep looping until valid terminal submission; only failure = 150% ceiling." Every surviving construct maps 1:1 to a clause of this principle.
2. **State collapse.** Three counters (`tool_overshoot`, `overshoot_units`, `text_only_no_terminal_turns`) collapse into one (`tool_calls_used`). Three rules collapse into one. Four exit reasons collapse into two.
3. **Structural invariants > runtime branches.** Mandatory terminals + mandatory budget eliminate the entire "non-terminal-capable agent" code path and the conditional gating it required.

**Alternatives considered.**
- **Iteration 3 — text-only hard stop with cold-start guard.** Rejected: adds a second counter and a second exit reason for a failure that has the same remediation as tool-overflow.
- **Two-rule design (`terminal_call_reminder` + `missing_terminal_reminder`).** Rejected: once the `tool_uses` filter is dropped, both rules have identical trigger conditions and identical bodies. No information value in splitting.
- **Staged-progression single rule (75/100/125%).** Rejected: adds `notification_state` complexity for a body the model can already read from the printed `used/limit/ceiling/turns_remaining`.
- **Keep `TEXT_RESPONSE` as a separate exit for non-terminal agents.** Rejected: with mandatory terminals invariant, the non-terminal-agent code path no longer exists. The enum value would be dead.
- **Keep `tool_call_limit: int | None`.** Rejected: invariant 2 lets the failure predicate drop the None-check (1 line vs 3). One more invariant, less runtime branching.

**Why chosen.** Minimal state, minimal predicates, minimal exit reasons, consistent with the operating principle. The exit decision is 5 lines. The transcript invariant and background-task cancellation are preserved.

**Consequences.**
- (+) ~14 named constructs deleted (`_ToolBudgetView`, `tool_budget`, `tool_overshoot`, `overshoot_units`, `_dispatch_final_message_tools`, `_attach_default_overshoot_rules`, three rule factories, two exit reasons, `max_tolerance_after_max_tool_call`, `text_only_no_terminal_turns`, `budget_overflow_reminder_every`).
- (+) 5-line exit decision in `_run_query_loop`.
- (+) Mandatory invariants make terminal-less agents and budget-less agents structurally impossible.
- (−) Pathological text-only-forever agents loop until cancelled by the caller. Accepted as out-of-scope for the engine layer.
- (−) Legacy YAML profiles carrying `max_tolerance_after_max_tool_call` fail Pydantic validation at boot. Intentional loud break.
- (−) Profiles without `terminals:` or `tool_call_limit:` fail validation at boot. Pre-implementation audit must retrofit every such profile.
- (−) `test_loop_resource_limit_transcript.py` and `test_soft_limit_behavior.py` need streaming-provider-fake rewrites. Tractable; prior art exists in the test suite.

**Follow-ups.**
- Audit timeout coverage on `run_ephemeral_agent` callers. Structural assertion + reminder make the infinite-text-only scenario theoretically reachable but bounded only by caller-side deadlines. Out of scope for this commit.
- `EngineConfig` becomes empty after this commit. Consider deleting the section entirely in a follow-up if no future field is anticipated. Out of scope.
- The reminder fires every turn after turn 1. Token cost is small (~50 tokens × ~10 turns ≈ 500 tokens per run). If real traces show excess noise, revisit with optional staged thresholds; not anticipated to be necessary.

---

## Consensus trail

- **Iteration 1** — Initial plan: tier-driven reminders + cold-start guard + two-commit split + two rules. Critic: ITERATE (8 precision items).
- **Iteration 2** — Cold-start guard formalized, dual-write contract for tier state. Architect: PROCEED-WITH-REVISIONS (5 items including critical hard-stop math defect — predicate was tier-coupled, not numeric).
- **Iteration 3** — Hard-stop math corrected to pure numeric 150%; exit reasons collapsed to one for failure. Critic: APPROVE.
- **Iteration 4** — User locked operating principle; design simplified further. Notification rules collapsed from 2 to 1; helper `_dispatch_final_message_tools` deleted; `TEXT_RESPONSE` removed; mandatory invariants on terminals + tool_call_limit introduced. Critic: **APPROVE**.

---

## Verified no orphan references (executor runs after commit)

```bash
grep -rn "max_tolerance_after_max_tool_call\|text_only_no_terminal_turns\|tool_overshoot\|overshoot_units\|_ToolBudgetView\|tool_budget\|RESOURCE_LIMIT\|TERMINAL_REFUSED\|TEXT_RESPONSE\|make_budget_warning\|make_budget_overflow_reminder\|make_missing_terminal_reminder\|budget_overflow_reminder_every\|_dispatch_final_message_tools\|_attach_default_overshoot_rules" backend/ docs/
# Expected: empty.

grep -rn "TERMINAL_NOT_SUBMITTED\|terminal_submission_failed\|make_terminal_call_reminder" backend/
# Expected: hits in context.py, loop.py, factories.py, factory.py, notification __init__.py x2, plus new/rewritten tests.

grep -L "^terminals:\|^tool_call_limit:" backend/src/agents/profile/**/*.md
# Expected: empty (every profile declares both fields).
```
