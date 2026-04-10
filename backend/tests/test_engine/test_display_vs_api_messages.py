"""Unit tests for the display_messages vs api_messages split.

Two invariants are verified:

1. ``compact_for_api`` is a pure function — it never mutates its input
   ``display_messages`` list, even when compaction kicks in.
2. The query loop's ``_build_background_reminder`` produces a regular
   ``ConversationMessage`` that can be appended to the durable history.

The compaction-path tests use a stub API client so no network calls are
made.
"""

from __future__ import annotations

import asyncio
import copy
from collections.abc import AsyncIterator
from typing import Any

import pytest

from compaction import SessionState, compact_for_api
from compaction.compactor import compact_conversation
from compaction.compactor import (
    AUTOCOMPACT_BUFFER_TOKENS,
    get_autocompact_threshold,
    reduce_for_api,
    _sanitize_tool_sequence,
)
from engine.core.query import _build_background_reminder
from engine.runtime.background_tasks import BackgroundTaskManager
from message.messages import (
    BackgroundTaskStateBlock,
    ConversationMessage,
    SystemReminderBlock,
    TextBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from providers.types import (
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    UsageSnapshot,
)
from tools.core.base import ToolResult
from tools.builtins.background._common import (
    build_background_snapshot_metadata,
    render_background_snapshot,
)


# ---------------------------------------------------------------------------
# Stub API client
# ---------------------------------------------------------------------------


class _StubApiClient:
    """Minimal stub that yields a single ApiMessageCompleteEvent.

    Used by ``compact_for_api`` when full compaction kicks in. The returned
    text is a fixed summary so tests can assert on it deterministically.
    """

    def __init__(self, summary: str = "<summary>STUB SUMMARY</summary>") -> None:
        self._summary = summary
        self.calls = 0

    async def stream_message(self, request: ApiMessageRequest) -> AsyncIterator[Any]:
        self.calls += 1
        msg = ConversationMessage(
            role="assistant", content=[TextBlock(text=self._summary)]
        )
        yield ApiMessageCompleteEvent(message=msg, usage=UsageSnapshot())


class _ToolPairValidatingApiClient(_StubApiClient):
    async def stream_message(self, request: ApiMessageRequest) -> AsyncIterator[Any]:
        pending_tool_uses: set[str] = set()
        for message in request.messages:
            if pending_tool_uses:
                tool_results = {
                    block.tool_use_id
                    for block in message.content
                    if isinstance(block, ToolResultBlock)
                }
                assert message.role == "user"
                assert pending_tool_uses <= tool_results
                pending_tool_uses = set()
            pending_tool_uses = {
                block.id
                for block in message.content
                if isinstance(block, ToolUseBlock)
            }
        assert not pending_tool_uses
        async for event in super().stream_message(request):
            yield event


def _make_user(text: str) -> ConversationMessage:
    return ConversationMessage.from_user_text(text)


# ---------------------------------------------------------------------------
# compact_for_api purity
# ---------------------------------------------------------------------------


class TestCompactForApiPurity:
    """compact_for_api must never mutate display_messages."""

    @pytest.mark.asyncio
    async def test_below_threshold_returns_fresh_list(self) -> None:
        display = [_make_user("hello"), _make_user("world")]
        snapshot = copy.deepcopy(display)

        api = await compact_for_api(
            display,
            api_client=_StubApiClient(),
            model="claude-opus-4-6",
            state=SessionState(),
        )

        # Pure: input unchanged
        assert display == snapshot
        # Returned a new list, not the same reference
        assert api is not display
        # Same content (no compaction needed)
        assert [m.text for m in api] == [m.text for m in display]

    @pytest.mark.asyncio
    async def test_does_not_mutate_when_compaction_triggers(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """Force compaction by stubbing should_autocompact, then verify
        the original display_messages list is byte-identical afterwards."""
        from compaction import compactor as compactor_mod

        display = [
            _make_user(f"message {i} with some content to count tokens")
            for i in range(20)
        ]
        snapshot = copy.deepcopy(display)
        original_id = id(display)

        # Force the compact path: first call says yes, second call says no
        # so microcompact-only short-circuits.
        calls: list[int] = []

        def fake_should(msgs, model, state):  # type: ignore[no-untyped-def]
            calls.append(len(msgs))
            return len(calls) == 1  # only the first check returns True

        monkeypatch.setattr(compactor_mod, "should_autocompact", fake_should)

        api = await compact_for_api(
            display,
            api_client=_StubApiClient(),
            model="claude-opus-4-6",
            state=SessionState(),
        )

        # Original list object unchanged in identity AND content.
        assert id(display) == original_id
        assert display == snapshot
        # api is a fresh list
        assert api is not display

    @pytest.mark.asyncio
    async def test_full_compact_returns_new_list_with_summary(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """When microcompact alone is insufficient, full compact runs and
        produces a summary-prefixed list. display_messages stays untouched."""
        from compaction import compactor as compactor_mod

        display = [_make_user(f"msg {i}") for i in range(15)]
        snapshot = copy.deepcopy(display)

        # Always say compaction is needed so the full path runs.
        monkeypatch.setattr(
            compactor_mod, "should_autocompact", lambda msgs, model, state: True
        )

        client = _StubApiClient(summary="<summary>compressed</summary>")
        api = await compact_for_api(
            display,
            api_client=client,
            model="claude-opus-4-6",
            state=SessionState(),
        )

        assert display == snapshot, "display_messages was mutated"
        assert client.calls == 1, "compact_conversation should call the API once"
        assert api is not display
        # The summary message replaces the older history.
        assert any("compressed" in m.text for m in api)

    @pytest.mark.asyncio
    async def test_compact_conversation_keeps_tool_pairs_out_of_split_boundary(
        self,
    ) -> None:
        messages = [
            _make_user("older context"),
            ConversationMessage(
                role="assistant",
                content=[
                    ToolUseBlock(id="toolu_pair", name="check_background_progress", input={"task_id": "bg_1"})
                ],
            ),
            ConversationMessage(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id="toolu_pair",
                        content="background snapshot",
                    )
                ],
            ),
            _make_user("newer context"),
        ]

        result = await compact_conversation(
            copy.deepcopy(messages),
            api_client=_ToolPairValidatingApiClient(),
            model="claude-opus-4-6",
            preserve_recent=2,
            skip_microcompact=True,
        )

        assert result is not messages
        assert any("STUB SUMMARY" in msg.text for msg in result)

    def test_sanitize_tool_sequence_drops_orphaned_tool_results(self) -> None:
        messages = [
            _make_user("prompt"),
            ConversationMessage(role="assistant", content=[TextBlock(text="no tools here")]),
            ConversationMessage(
                role="user",
                content=[ToolResultBlock(tool_use_id="toolu_orphan", content="stale result")],
            ),
        ]

        sanitized = _sanitize_tool_sequence(messages)

        assert len(sanitized) == 2
        assert all(
            not any(isinstance(block, ToolResultBlock) for block in msg.content)
            for msg in sanitized
        )

    @pytest.mark.asyncio
    async def test_compact_conversation_rejects_orphan_tool_result_history(
        self,
    ) -> None:
        client = _StubApiClient()
        messages = [
            _make_user("older context"),
            ConversationMessage(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id="toolu_orphan",
                        content="orphaned tool result",
                    )
                ],
            ),
            _make_user("newer context"),
        ]

        with pytest.raises(ValueError, match="compaction preflight rejected malformed tool sequencing"):
            await compact_conversation(
                copy.deepcopy(messages),
                api_client=client,
                model="claude-opus-4-6",
                preserve_recent=0,
                skip_microcompact=True,
            )

        assert client.calls == 0, "preflight should fail before the summary API call"

    @pytest.mark.asyncio
    async def test_compact_for_api_sanitizes_invalid_history_before_provider_call(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        from compaction import compactor as compactor_mod

        monkeypatch.setattr(
            compactor_mod, "should_autocompact", lambda msgs, model, state: True
        )

        display = [
            _make_user("prefix 0"),
            _make_user("prefix 1"),
            _make_user("older context"),
            ConversationMessage(
                role="assistant",
                content=[ToolUseBlock(id="toolu_pair", name="echo", input={"value": "x"})],
            ),
            ConversationMessage(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id="toolu_pair",
                        content="expected tool result",
                    ),
                    ToolResultBlock(
                        tool_use_id="toolu_orphan",
                        content="unexpected extra tool result",
                    ),
                ],
            ),
            _make_user("suffix 0"),
            _make_user("suffix 1"),
            _make_user("suffix 2"),
            _make_user("newer context"),
        ]
        state = SessionState()
        client = _StubApiClient()

        api = await compact_for_api(
            display,
            api_client=client,
            model="claude-opus-4-6",
            state=state,
        )

        assert client.calls == 1, "sanitized history should remain compactable"
        assert any("STUB SUMMARY" in msg.text for msg in api)
        assert state.compacted is True
        assert state.consecutive_failures == 0


# ---------------------------------------------------------------------------
# Background reminder builder
# ---------------------------------------------------------------------------


class TestBuildBackgroundReminder:
    """The reminder is a regular ConversationMessage that lives in
    display_messages — verify it's well-formed and can be appended."""

    def test_returns_none_when_no_pending_tasks(self) -> None:
        mgr = BackgroundTaskManager()
        assert _build_background_reminder(mgr) is None

    @pytest.mark.asyncio
    async def test_includes_task_id_and_label(self) -> None:
        mgr = BackgroundTaskManager()

        async def _coro() -> ToolResult:
            await asyncio.sleep(10)
            return ToolResult(output="done")

        mgr.launch(
            "bg_1",
            "daytona_bash",
            {"command": "sleep 10"},
            _coro(),
            task_note="long sleep",
        )
        # Append a progress line so get_reminder_diff returns something.
        mgr.append_progress("bg_1", "halfway there")

        msg = _build_background_reminder(mgr)
        assert msg is not None
        # Reminder is carried as a BackgroundTaskStateBlock, not user text.
        assert msg.text == ""
        reminder_text = msg.background_task_state_text
        assert "bg_1" in reminder_text
        assert "long sleep" in reminder_text
        assert "halfway there" in reminder_text
        assert "Keep working on any other ready analysis or tool tasks first" in reminder_text
        assert "Only wait when this background task is the remaining blocker" in reminder_text
        assert msg.background_task_states[0].status == "running"
        api_param = msg.to_api_param()
        assert "<background-task" in api_param["content"][0]["text"]

        # Cursor advanced — second call has no new lines.
        msg2 = _build_background_reminder(mgr)
        assert msg2 is not None
        assert "halfway there" not in msg2.background_task_state_text
        assert "No new output" in msg2.background_task_state_text
        assert "Only wait when this background task is the remaining blocker" in (
            msg2.background_task_state_text
        )

        await mgr.cancel_all()

    @pytest.mark.asyncio
    async def test_appendable_to_display_messages_list(self) -> None:
        """Sanity check: the reminder can be appended and survives as a
        regular message (it is NOT a separate ephemeral type)."""
        mgr = BackgroundTaskManager()

        async def _coro() -> ToolResult:
            await asyncio.sleep(10)
            return ToolResult(output="done")

        mgr.launch("bg_1", "tool", {}, _coro())
        display: list[ConversationMessage] = [_make_user("hi")]

        reminder = _build_background_reminder(mgr)
        assert reminder is not None
        display.append(reminder)

        assert len(display) == 2
        assert display[1].role == "user"
        # The reminder lives as a BackgroundTaskStateBlock — not a TextBlock — so
        # display layers can render / filter it distinctly from real user
        # text.
        assert len(display[1].background_task_states) == 1
        assert display[1].text == ""

        await mgr.cancel_all()


# ---------------------------------------------------------------------------
# Threshold sanity (regression guard)
# ---------------------------------------------------------------------------


class TestThresholdSanity:
    def test_threshold_is_positive(self) -> None:
        threshold = get_autocompact_threshold("claude-opus-4-6")
        assert threshold > 0
        assert threshold > AUTOCOMPACT_BUFFER_TOKENS


# ---------------------------------------------------------------------------
# SystemReminderBlock primitives
# ---------------------------------------------------------------------------


class TestSystemReminderBlock:
    """The block must round-trip through pydantic and serialize to a wire
    format that Anthropic's API will accept (a plain text block with
    <system-reminder> tags around the body)."""

    def test_block_construction_and_defaults(self) -> None:
        block = SystemReminderBlock(text="hello")
        assert block.type == "system_reminder"
        assert block.text == "hello"
        assert block.category == ""

        categorized = SystemReminderBlock(text="x", category="background_progress")
        assert categorized.category == "background_progress"

    def test_message_with_reminder_text_excludes_reminder(self) -> None:
        """ConversationMessage.text returns only TextBlock content — never
        SystemReminderBlock — so display layers can render reminders
        separately from real user text."""
        msg = ConversationMessage(
            role="user",
            content=[
                TextBlock(text="hi"),
                SystemReminderBlock(text="background bg_1 still running"),
            ],
        )
        assert msg.text == "hi"
        assert msg.system_reminder_text == "background bg_1 still running"
        assert len(msg.system_reminders) == 1

    def test_to_api_param_wraps_in_tags(self) -> None:
        """Wire serialization: SystemReminderBlock becomes a text block with
        <system-reminder>...</system-reminder> tags around the body so the
        provider sees a normal text block but the model recognises the
        engine-generated convention."""
        msg = ConversationMessage(
            role="user",
            content=[SystemReminderBlock(text="bg_1 done", category="x")],
        )
        api = msg.to_api_param()
        assert api["role"] == "user"
        assert len(api["content"]) == 1
        block = api["content"][0]
        assert block["type"] == "text"
        assert block["text"] == "<system-reminder>\nbg_1 done\n</system-reminder>"

    def test_to_api_param_mixed_content_preserves_order(self) -> None:
        msg = ConversationMessage(
            role="user",
            content=[
                TextBlock(text="user said"),
                SystemReminderBlock(text="reminder"),
                TextBlock(text="more"),
            ],
        )
        api = msg.to_api_param()
        types = [b["type"] for b in api["content"]]
        assert types == ["text", "text", "text"]
        assert api["content"][0]["text"] == "user said"
        assert "<system-reminder>" in api["content"][1]["text"]
        assert api["content"][2]["text"] == "more"

    def test_pydantic_round_trip(self) -> None:
        """Discriminated union must accept the new block type when loading
        a serialized ConversationMessage from JSON (e.g. from the DB)."""
        original = ConversationMessage(
            role="user",
            content=[
                SystemReminderBlock(text="hi", category="background_progress"),
            ],
        )
        dumped = original.model_dump()
        restored = ConversationMessage.model_validate(dumped)
        assert len(restored.content) == 1
        block = restored.content[0]
        assert isinstance(block, SystemReminderBlock)
        assert block.text == "hi"
        assert block.category == "background_progress"

    def test_empty_reminder_text(self) -> None:
        """Empty reminders are valid (a degenerate but legal state)."""
        block = SystemReminderBlock(text="")
        msg = ConversationMessage(role="user", content=[block])
        api = msg.to_api_param()
        assert api["content"][0]["text"] == "<system-reminder>\n\n</system-reminder>"

    def test_multiple_reminders_in_one_message(self) -> None:
        msg = ConversationMessage(
            role="user",
            content=[
                SystemReminderBlock(text="first", category="bg"),
                SystemReminderBlock(text="second", category="warn"),
            ],
        )
        assert len(msg.system_reminders) == 2
        assert msg.system_reminder_text == "first\nsecond"
        api = msg.to_api_param()
        assert len(api["content"]) == 2
        assert "first" in api["content"][0]["text"]
        assert "second" in api["content"][1]["text"]


# ---------------------------------------------------------------------------
# compact_for_api — state and error handling
# ---------------------------------------------------------------------------


class _CrashingApiClient:
    """API client whose stream_message always raises."""

    async def stream_message(self, request: ApiMessageRequest) -> AsyncIterator[Any]:
        raise RuntimeError("simulated provider failure")
        yield  # pragma: no cover — make it an async generator


class TestCompactForApiState:
    """Verify SessionState bookkeeping and failure handling."""

    @pytest.mark.asyncio
    async def test_state_unchanged_when_below_threshold(self) -> None:
        display = [_make_user("hi")]
        state = SessionState(
            compacted=False, turn_counter=0, consecutive_failures=0
        )

        await compact_for_api(
            display,
            api_client=_StubApiClient(),
            model="claude-opus-4-6",
            state=state,
        )

        assert state.compacted is False
        assert state.turn_counter == 0
        assert state.consecutive_failures == 0

    @pytest.mark.asyncio
    async def test_state_marks_compacted_after_full_compact(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        from compaction import compactor as compactor_mod

        monkeypatch.setattr(
            compactor_mod, "should_autocompact", lambda msgs, model, state: True
        )
        display = [_make_user(f"m {i}") for i in range(10)]
        state = SessionState(
            compacted=False, turn_counter=0, consecutive_failures=0
        )

        await compact_for_api(
            display,
            api_client=_StubApiClient(),
            model="claude-opus-4-6",
            state=state,
        )

        assert state.compacted is True
        assert state.turn_counter == 1
        assert state.consecutive_failures == 0

    @pytest.mark.asyncio
    async def test_failure_increments_consecutive_failures(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """When the API call inside compact_conversation crashes,
        compact_for_api swallows the exception, increments
        consecutive_failures, and returns the microcompacted working
        list (NOT the original display_messages)."""
        from compaction import compactor as compactor_mod

        monkeypatch.setattr(
            compactor_mod, "should_autocompact", lambda msgs, model, state: True
        )
        display = [_make_user(f"m {i}") for i in range(10)]
        snapshot = copy.deepcopy(display)
        state = SessionState(
            compacted=False, turn_counter=5, consecutive_failures=1
        )

        result = await compact_for_api(
            display,
            api_client=_CrashingApiClient(),
            model="claude-opus-4-6",
            state=state,
        )

        # display_messages still untouched
        assert display == snapshot
        # State reflects the failure
        assert state.compacted is False
        assert state.turn_counter == 5  # NOT incremented on failure
        assert state.consecutive_failures == 2
        # Returned a fresh list (not the original)
        assert result is not display

    @pytest.mark.asyncio
    async def test_preserves_background_state_blocks_in_recent_window(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """A recent background state must survive the
        compaction round-trip when it falls within preserve_recent."""
        from compaction import compactor as compactor_mod

        monkeypatch.setattr(
            compactor_mod, "should_autocompact", lambda msgs, model, state: True
        )
        # Build a long history with the reminder at the end (recent).
        display: list[ConversationMessage] = [
            _make_user(f"old msg {i}") for i in range(30)
        ]
        reminder_msg = ConversationMessage(
            role="user",
            content=[
                BackgroundTaskStateBlock(
                    task_id="bg_1",
                    tool_name="run_subagent",
                    task_type="subagent",
                    status="running",
                    source="engine_progress",
                    text="bg_1 still running",
                )
            ],
        )
        display.append(reminder_msg)

        api = await compact_for_api(
            display,
            api_client=_StubApiClient(),
            model="claude-opus-4-6",
            state=SessionState(),
        )

        # The reminder must appear in the compacted output.
        all_reminders = [r for m in api for r in m.background_task_states]
        assert any(
            "bg_1 still running" in r.text for r in all_reminders
        ), f"Reminder lost in compaction. api content: {[m.model_dump() for m in api]}"

    def test_reduce_for_api_drops_stale_snapshot_pairs_but_not_display_history(self) -> None:
        display = [
            ConversationMessage(
                role="assistant",
                content=[
                    ToolUseBlock(id="toolu_1", name="check_background_progress", input={"task_id": "all"})
                ],
            ),
            ConversationMessage(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id="toolu_1",
                        content=render_background_snapshot(
                            "progress",
                            [{"task_id": "bg_1", "status": "running", "output": "old"}],
                        ),
                        metadata=build_background_snapshot_metadata(
                            "progress",
                            "all",
                            [{"task_id": "bg_1", "status": "running", "output": "old"}],
                        ),
                    )
                ],
            ),
            ConversationMessage(
                role="user",
                content=[
                    BackgroundTaskStateBlock(
                        task_id="bg_1",
                        tool_name="run_subagent",
                        task_type="subagent",
                        status="completed",
                        source="engine_terminal",
                        text="done",
                    )
                ],
            ),
        ]
        snapshot = copy.deepcopy(display)

        api = reduce_for_api(display)

        assert display == snapshot
        assert any(
            isinstance(block, ToolUseBlock) and block.id == "toolu_1"
            for msg in display
            for block in msg.content
        )
        assert all(
            not any(isinstance(block, ToolUseBlock) and block.id == "toolu_1" for block in msg.content)
            for msg in api
        )


# ---------------------------------------------------------------------------
# _build_background_reminder — multi-task and lifecycle behaviour
# ---------------------------------------------------------------------------


class TestBuildReminderEdgeCases:
    """Cover multi-task ordering, completed-task filtering, and the
    no-progress-since-last-call branch."""

    @pytest.mark.asyncio
    async def test_multiple_pending_tasks_all_appear(self) -> None:
        mgr = BackgroundTaskManager()

        async def _coro() -> ToolResult:
            await asyncio.sleep(10)
            return ToolResult(output="done")

        mgr.launch("bg_1", "tool_a", {}, _coro(), task_note="first task")
        mgr.launch("bg_2", "tool_b", {}, _coro(), task_note="second task")
        mgr.append_progress("bg_1", "alpha")
        mgr.append_progress("bg_2", "beta")

        msg = _build_background_reminder(mgr)
        assert msg is not None
        text = msg.background_task_state_text
        assert "bg_1" in text and "first task" in text and "alpha" in text
        assert "bg_2" in text and "second task" in text and "beta" in text

        await mgr.cancel_all()

    @pytest.mark.asyncio
    async def test_completed_tasks_excluded(self) -> None:
        """Reminder body should only mention currently-running tasks."""
        mgr = BackgroundTaskManager()

        async def _quick() -> ToolResult:
            return ToolResult(output="finished")

        async def _slow() -> ToolResult:
            await asyncio.sleep(10)
            return ToolResult(output="done")

        mgr.launch("bg_done", "tool_quick", {}, _quick(), task_note="quick")
        mgr.launch("bg_running", "tool_slow", {}, _slow(), task_note="slow")
        # Let the quick task finish.
        await asyncio.sleep(0.05)

        msg = _build_background_reminder(mgr)
        assert msg is not None
        text = msg.background_task_state_text
        assert "bg_running" in text
        assert "bg_done" not in text, "completed task should be filtered out"

        await mgr.cancel_all()

    @pytest.mark.asyncio
    async def test_no_progress_branch_uses_seconds_since_format(self) -> None:
        """Tasks with no new progress lines render the 'No new output' body."""
        mgr = BackgroundTaskManager()

        async def _coro() -> ToolResult:
            await asyncio.sleep(10)
            return ToolResult(output="done")

        mgr.launch("bg_x", "tool", {}, _coro())
        # Don't append any progress lines. The startup-stamp line counts as
        # initial progress, so the FIRST reminder will include it.
        first = _build_background_reminder(mgr)
        assert first is not None
        # Cursor advanced — second call has nothing new.
        second = _build_background_reminder(mgr)
        assert second is not None
        assert "No new output" in second.background_task_state_text

        await mgr.cancel_all()


# ---------------------------------------------------------------------------
# ConversationMessage interaction with mixed content
# ---------------------------------------------------------------------------


class TestConversationMessageMixed:
    """Verify SystemReminderBlock plays nicely with all other content
    block types and does not interfere with their accessors."""

    def test_text_property_only_returns_text_blocks(self) -> None:
        from message.messages import ToolUseBlock

        msg = ConversationMessage(
            role="assistant",
            content=[
                TextBlock(text="hello"),
                ToolUseBlock(id="t1", name="bash", input={"cmd": "ls"}),
                TextBlock(text=" world"),
            ],
        )
        assert msg.text == "hello world"
        assert msg.system_reminder_text == ""
        assert msg.system_reminders == []

    def test_tool_uses_property_unaffected(self) -> None:
        from message.messages import ToolUseBlock

        msg = ConversationMessage(
            role="assistant",
            content=[
                ToolUseBlock(id="t1", name="bash", input={"cmd": "ls"}),
                SystemReminderBlock(text="ignore me"),
            ],
        )
        assert len(msg.tool_uses) == 1
        assert msg.tool_uses[0].name == "bash"

    def test_pydantic_discriminator_distinguishes_text_vs_reminder(self) -> None:
        """Round-tripping a message with both TextBlock and
        SystemReminderBlock must preserve the distinction (the discriminator
        on type=... is what makes this work)."""
        original = ConversationMessage(
            role="user",
            content=[
                TextBlock(text="real user input"),
                SystemReminderBlock(text="engine note"),
            ],
        )
        dumped = original.model_dump()
        restored = ConversationMessage.model_validate(dumped)
        assert isinstance(restored.content[0], TextBlock)
        assert isinstance(restored.content[1], SystemReminderBlock)
