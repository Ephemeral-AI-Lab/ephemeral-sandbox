"""Unit tests for durable message history vs provider message views."""

from __future__ import annotations

import copy

from engine.background.history import reduce_background_task_history
from engine.query.provider_history import build_provider_messages, sanitize_tool_sequence
from message.message import (
    Message,
    SystemNotificationBlock,
    TextBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from tools.background._lib.task_output import (
    build_background_snapshot_metadata,
    render_background_snapshot,
)


def _user(text: str) -> Message:
    return Message.from_user_text(text)


def _assistant(text: str) -> Message:
    return Message(role="assistant", content=[TextBlock(text=text)])


def _tool_use(id: str, name: str, input: dict) -> Message:  # noqa: A002
    return Message(
        role="assistant",
        content=[ToolUseBlock(tool_use_id=id, name=name, input=input)],
    )


def _tool_result(tool_use_id: str, content: str) -> Message:
    return Message(
        role="user",
        content=[ToolResultBlock(tool_use_id=tool_use_id, content=content)],
    )


class TestPrepareProviderMessages:
    """Provider-history preparation must not mutate the display transcript."""

    def test_provider_copy_drops_tool_result_metadata_without_deepcopy(self) -> None:
        class NoDeepcopy:
            def __deepcopy__(self, memo: dict[int, object]) -> object:
                raise AssertionError("provider view must not deepcopy tool metadata")

        sentinel = NoDeepcopy()
        display = [
            _tool_use("toolu_pair", "echo", {"value": "x"}),
            Message(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id="toolu_pair",
                        content="result",
                        metadata={
                            "heavy": sentinel,
                            "hook_trace": [
                                {
                                    "status": "fail",
                                    "metadata": {"reason": "blocked"},
                                }
                            ],
                        },
                    )
                ],
            ),
        ]

        provider = build_provider_messages(display)

        result = next(
            block
            for msg in provider
            for block in msg.content
            if isinstance(block, ToolResultBlock)
        )
        assert result.metadata == {
            "hook_trace": [
                {
                    "status": "fail",
                    "metadata": {"reason": "blocked"},
                }
            ]
        }
        original = display[1].content[0]
        assert isinstance(original, ToolResultBlock)
        assert original.metadata["heavy"] is sentinel

    def test_returns_fresh_provider_list(self) -> None:
        display = [_user("hello"), _user("world")]
        snapshot = copy.deepcopy(display)

        provider = build_provider_messages(display)

        assert display == snapshot
        assert provider is not display
        assert provider[0] is not display[0]
        assert [m.assistant_text for m in provider] == [m.assistant_text for m in display]

    def test_sanitize_tool_sequence_drops_orphaned_tool_results(self) -> None:
        messages = [
            _user("prompt"),
            _assistant("no tools here"),
            _tool_result("toolu_orphan", "stale result"),
        ]

        sanitized = sanitize_tool_sequence(messages)

        assert len(sanitized) == 2
        assert all(
            not any(isinstance(block, ToolResultBlock) for block in msg.content)
            for msg in sanitized
        )

    def test_prepare_provider_messages_sanitizes_invalid_history(self) -> None:
        display = [
            _user("older context"),
            _tool_use("toolu_pair", "echo", {"value": "x"}),
            Message(
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
            _user("newer context"),
        ]

        provider = build_provider_messages(display)

        result_ids = {
            block.tool_use_id
            for msg in provider
            for block in msg.content
            if isinstance(block, ToolResultBlock)
        }
        assert result_ids == {"toolu_pair"}

    def test_reduce_background_task_history_drops_stale_snapshot_pairs(self) -> None:
        old_statuses = [{"task_id": "bg_1", "status": "running", "output": "old"}]
        new_statuses = [{"task_id": "bg_1", "status": "completed", "output": "done"}]
        display = [
            _tool_use("toolu_old", "wait_background_tasks", {"task_id": "all"}),
            Message(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id="toolu_old",
                        content=render_background_snapshot("progress", old_statuses),
                        metadata=build_background_snapshot_metadata(
                            "progress",
                            "all",
                            old_statuses,
                        ),
                    )
                ],
            ),
            _tool_use("toolu_new", "wait_background_tasks", {"task_id": "all"}),
            Message(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id="toolu_new",
                        content=render_background_snapshot("wait_completed", new_statuses),
                        metadata=build_background_snapshot_metadata(
                            "wait_completed",
                            "all",
                            new_statuses,
                        ),
                    )
                ],
            ),
        ]
        snapshot = copy.deepcopy(display)

        provider = reduce_background_task_history(display)

        assert display == snapshot
        assert any(
            isinstance(block, ToolUseBlock) and block.tool_use_id == "toolu_old"
            for msg in display
            for block in msg.content
        )
        assert all(
            not any(
                isinstance(block, ToolUseBlock) and block.tool_use_id == "toolu_old"
                for block in msg.content
            )
            for msg in provider
        )
        assert any(
            isinstance(block, ToolUseBlock) and block.tool_use_id == "toolu_new"
            for msg in provider
            for block in msg.content
        )

    def test_reduce_background_task_history_prefers_finished_snapshot(self) -> None:
        running_statuses = [
            {
                "task_id": "bg_1",
                "tool_name": "run_subagent",
                "task_type": "subagent",
                "status": "running",
                "output": "Working.",
            }
        ]
        statuses = [
            {
                "task_id": "bg_1",
                "tool_name": "run_subagent",
                "task_type": "subagent",
                "status": "finished",
                "output": '{"summary": "Posted."}',
            }
        ]
        display = [
            _tool_use("toolu_running", "wait_background_tasks", {"task_id": "all"}),
            Message(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id="toolu_running",
                        content=render_background_snapshot("wait_timed_out", running_statuses),
                        metadata=build_background_snapshot_metadata(
                            "wait_timed_out",
                            "all",
                            running_statuses,
                        ),
                    )
                ],
            ),
            _tool_use("toolu_wait", "wait_background_tasks", {"task_id": "all"}),
            Message(
                role="user",
                content=[
                    ToolResultBlock(
                        tool_use_id="toolu_wait",
                        content=render_background_snapshot("wait_no_tasks", statuses),
                        metadata=build_background_snapshot_metadata(
                            "wait_no_tasks",
                            "all",
                            statuses,
                        ),
                    )
                ],
            ),
        ]

        provider = reduce_background_task_history(display)

        assert any(
            isinstance(block, ToolUseBlock) and block.tool_use_id == "toolu_wait"
            for msg in provider
            for block in msg.content
        )
        result_contents = [
            block.content
            for msg in provider
            for block in msg.content
            if isinstance(block, ToolResultBlock)
        ]
        assert any("[NO TASKS]" in content for content in result_contents)
        assert all(
            not any(
                isinstance(block, ToolUseBlock) and block.tool_use_id == "toolu_running"
                for block in msg.content
            )
            for msg in provider
        )


class TestSystemNotificationBlock:
    """SystemNotificationBlock must round-trip and serialize as provider text."""

    def test_block_construction_and_defaults(self) -> None:
        block = SystemNotificationBlock(text="hello")
        assert block.type == "system_notification"
        assert block.text == "hello"

    def test_message_with_notification_text_excludes_notification(self) -> None:
        msg = Message(
            role="user",
            content=[
                TextBlock(text="hi"),
                SystemNotificationBlock(text="background bg_1 still running"),
            ],
        )
        assert msg.assistant_text == "hi"
        assert msg.system_notification_text == "background bg_1 still running"
        assert len(msg.system_notifications) == 1

    def test_to_api_param_wraps_in_tags(self) -> None:
        msg = Message(
            role="user",
            content=[SystemNotificationBlock(text="bg_1 done")],
        )
        api = msg.to_api_param()
        assert api["role"] == "user"
        assert len(api["content"]) == 1
        block = api["content"][0]
        assert block["type"] == "text"
        assert block["text"] == "<system-reminder>\nbg_1 done\n</system-reminder>"

    def test_to_api_param_mixed_content_preserves_order(self) -> None:
        msg = Message(
            role="user",
            content=[
                TextBlock(text="user said"),
                SystemNotificationBlock(text="notification"),
                TextBlock(text="more"),
            ],
        )
        api = msg.to_api_param()
        types = [block["type"] for block in api["content"]]
        assert types == ["text", "text", "text"]
        assert api["content"][0]["text"] == "user said"
        assert "<system-reminder>" in api["content"][1]["text"]
        assert api["content"][2]["text"] == "more"

    def test_pydantic_round_trip(self) -> None:
        original = Message(
            role="user",
            content=[
                SystemNotificationBlock(text="hi"),
            ],
        )
        dumped = original.model_dump()
        restored = Message.model_validate(dumped)
        assert len(restored.content) == 1
        block = restored.content[0]
        assert isinstance(block, SystemNotificationBlock)
        assert block.text == "hi"

    def test_empty_notification_text(self) -> None:
        block = SystemNotificationBlock(text="")
        msg = Message(role="user", content=[block])
        api = msg.to_api_param()
        assert api["content"][0]["text"] == "<system-reminder>\n\n</system-reminder>"

    def test_multiple_notifications_in_one_message(self) -> None:
        msg = Message(
            role="user",
            content=[
                SystemNotificationBlock(text="first"),
                SystemNotificationBlock(text="second"),
            ],
        )
        assert len(msg.system_notifications) == 2
        assert msg.system_notification_text == "first\nsecond"
        api = msg.to_api_param()
        assert len(api["content"]) == 2
        assert "first" in api["content"][0]["text"]
        assert "second" in api["content"][1]["text"]

class TestConversationMessageMixed:
    """SystemNotificationBlock must not interfere with other block accessors."""

    def test_text_property_only_returns_text_blocks(self) -> None:
        msg = Message(
            role="assistant",
            content=[
                TextBlock(text="hello"),
                ToolUseBlock(tool_use_id="t1", name="bash", input={"cmd": "ls"}),
                TextBlock(text=" world"),
            ],
        )
        assert msg.assistant_text == "hello world"
        assert msg.system_notification_text == ""
        assert msg.system_notifications == []

    def test_tool_uses_property_unaffected(self) -> None:
        msg = Message(
            role="assistant",
            content=[
                ToolUseBlock(tool_use_id="t1", name="bash", input={"cmd": "ls"}),
                SystemNotificationBlock(text="ignore me"),
            ],
        )
        assert len(msg.tool_uses) == 1
        assert msg.tool_uses[0].name == "bash"

    def test_pydantic_discriminator_distinguishes_text_vs_notification(self) -> None:
        original = Message(
            role="user",
            content=[
                TextBlock(text="real user input"),
                SystemNotificationBlock(text="engine note"),
            ],
        )
        dumped = original.model_dump()
        restored = Message.model_validate(dumped)
        assert isinstance(restored.content[0], TextBlock)
        assert isinstance(restored.content[1], SystemNotificationBlock)
