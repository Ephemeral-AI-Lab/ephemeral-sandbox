"""Built-in hooks for the SWE-EVO live e2e framework.

Real implementations:

- ``count_events`` — increments ``state.flags['count_<name>']`` per match.
- ``capture_prompt`` — copies the agent ``*_INVOKED`` payload into ``state.flags``.
- ``assert_event_sequence`` — checks ``state.seen_events`` contains the expected
  EventType subsequence at ``RUN_COMPLETED`` time.

Stubs deferred to next phase per plan §10:

- ``fail_evaluator_at`` — needs hook-injected scenario response replacement.
- ``assert_squash_after_n_edits`` — needs the squash event surface (see
  ``audit/squash_detection.md``).
"""

from __future__ import annotations

from task_center_runner.audit.events import Event, EventType
from task_center_runner.hooks.registry import (
    Hook,
    HookResult,
    MutableMockState,
)


_ROLE_TO_INVOKED: dict[str, EventType] = {
    "planner": EventType.PLANNER_INVOKED,
    "executor": EventType.EXECUTOR_INVOKED,
    "verifier": EventType.VERIFIER_INVOKED,
    "evaluator": EventType.EVALUATOR_INVOKED,
}


def count_events(event_type: EventType, *, name: str | None = None) -> Hook:
    """Return a Hook that increments ``state.flags[f'count_{name}']`` on each match."""
    counter_name = name or event_type.value

    def _fn(_event: Event, state: MutableMockState) -> HookResult:
        key = f"count_{counter_name}"
        new_value = int(state.flags.get(key, 0)) + 1
        state.flags[key] = new_value
        return HookResult(name=f"count_events:{counter_name}", extras={"count": new_value})

    return Hook(
        name=f"count_events:{counter_name}",
        event=event_type,
        when="post",
        fn=_fn,
    )


def capture_prompt(role: str, *, name: str | None = None) -> Hook:
    """Capture the *_INVOKED event payload for *role* into ``state.flags``."""
    if role not in _ROLE_TO_INVOKED:
        raise ValueError(f"Unknown role for capture_prompt: {role!r}")
    capture_name = name or role
    invoked_event = _ROLE_TO_INVOKED[role]

    def _fn(event: Event, state: MutableMockState) -> HookResult:
        state.flags[f"prompt_{capture_name}"] = dict(event.payload or {})
        return HookResult(name=f"capture_prompt:{capture_name}")

    return Hook(
        name=f"capture_prompt:{capture_name}",
        event=invoked_event,
        when="pre",
        fn=_fn,
    )


def assert_event_sequence(
    expected: list[EventType],
    *,
    name: str = "sequence",
) -> Hook:
    """Return a Hook fired on RUN_COMPLETED that asserts a subsequence match."""

    def _fn(_event: Event, state: MutableMockState) -> HookResult:
        seen = list(state.seen_events)
        idx = 0
        for needle in expected:
            while idx < len(seen) and seen[idx] != needle:
                idx += 1
            if idx == len(seen):
                return HookResult(
                    name=f"assert_event_sequence:{name}",
                    asserted=False,
                    failed_reason=(
                        f"Missing {needle.value!r} in seen events after position {idx}"
                    ),
                    extras={"seen": [e.value for e in seen]},
                )
            idx += 1
        return HookResult(name=f"assert_event_sequence:{name}", asserted=True)

    return Hook(
        name=f"assert_event_sequence:{name}",
        event=EventType.RUN_COMPLETED,
        when="post",
        fn=_fn,
    )


def fail_evaluator_at(*, attempt_seq: int = 1) -> Hook:
    raise NotImplementedError(
        f"fail_evaluator_at(attempt_seq={attempt_seq}) deferred to next phase per plan §10"
    )


def fail_verifier_at(*, checkpoint: str) -> Hook:
    """Inject a one-shot verifier failure when *checkpoint* is invoked."""

    def _fn(event: Event, state: MutableMockState) -> HookResult:
        if str(event.payload.get("checkpoint") or "") != checkpoint:
            return HookResult(
                name=f"fail_verifier_at:{checkpoint}",
                asserted=True,
                extras={"armed": False},
            )
        attempt_id = event.node.attempt_id or "*"
        state.inject_failure(
            role="verifier",
            attempt_id=attempt_id,
            checkpoint=checkpoint,
        )
        return HookResult(
            name=f"fail_verifier_at:{checkpoint}",
            asserted=True,
            extras={"armed": True, "attempt_id": attempt_id},
        )

    return Hook(
        name=f"fail_verifier_at:{checkpoint}",
        event=EventType.VERIFIER_INVOKED,
        when="post",
        fn=_fn,
    )


def assert_guard_after_wave(*, wave_id: str) -> Hook:
    """Assert a verifier guard for *wave_id* has executor dependencies."""

    def _fn(event: Event, _state: MutableMockState) -> HookResult:
        if str(event.payload.get("wave_id") or "") != wave_id:
            return HookResult(
                name=f"assert_guard_after_wave:{wave_id}",
                asserted=True,
                extras={"matched": False},
            )
        dependency_count = int(event.payload.get("dependency_count") or 0)
        ok = dependency_count > 0
        return HookResult(
            name=f"assert_guard_after_wave:{wave_id}",
            asserted=ok,
            failed_reason=None if ok else "guard had no dependencies",
            extras={"dependency_count": dependency_count, "matched": True},
        )

    return Hook(
        name=f"assert_guard_after_wave:{wave_id}",
        event=EventType.VERIFIER_SUCCESS,
        when="post",
        fn=_fn,
    )


def assert_recursive_workflow_closed_before_parent_guard() -> Hook:
    """Assert the parent recursive-return guard runs after close-report delivery."""

    def _fn(event: Event, state: MutableMockState) -> HookResult:
        if str(event.payload.get("checkpoint") or "") != "recursive_return":
            return HookResult(
                name="assert_recursive_workflow_closed_before_parent_guard",
                asserted=True,
                extras={"matched": False},
            )
        seen = list(state.seen_events)
        try:
            close_idx = seen.index(EventType.RECURSIVE_WORKFLOW_COMPLETED)
            guard_idx = len(seen) - 1
        except ValueError:
            return HookResult(
                name="assert_recursive_workflow_closed_before_parent_guard",
                asserted=False,
                failed_reason="recursive completion event was not observed",
                extras={"seen": [item.value for item in seen]},
            )
        ok = close_idx < guard_idx
        return HookResult(
            name="assert_recursive_workflow_closed_before_parent_guard",
            asserted=ok,
            failed_reason=None if ok else "parent guard preceded recursive completion",
            extras={"matched": True, "close_idx": close_idx, "guard_idx": guard_idx},
        )

    return Hook(
        name="assert_recursive_workflow_closed_before_parent_guard",
        event=EventType.VERIFIER_SUCCESS,
        when="post",
        fn=_fn,
    )


def assert_squash_after_n_edits(n: int = 16) -> Hook:
    raise NotImplementedError(
        f"assert_squash_after_n_edits(n={n}) deferred to next phase — "
        "see task_center_runner/audit/squash_detection.md"
    )


__all__ = [
    "assert_event_sequence",
    "assert_guard_after_wave",
    "assert_recursive_workflow_closed_before_parent_guard",
    "assert_squash_after_n_edits",
    "capture_prompt",
    "count_events",
    "fail_evaluator_at",
    "fail_verifier_at",
]
