"""Tests for tool registration, schema generation, and the tool call loop."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from pydantic import BaseModel, Field

from message import ConversationMessage, TextBlock, ToolUseBlock
from engine.core.query import QueryContext, _execute_tool_call, _launch_background_tool, run_query
from engine.runtime.background_tasks import BackgroundTaskManager
from message.stream_events import (
    AssistantTurnComplete,
    ToolExecutionCompleted,
    ToolExecutionStarted,
)
from providers.types import (
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiTextDeltaEvent,
    ApiToolUseDeltaEvent,
    UsageSnapshot,
)
from tools.core.base import (
    BaseTool,
    BaseToolkit,
    ExecutionMetadata,
    ToolExecutionContext,
    ToolRegistry,
    ToolResult,
)


# ---------------------------------------------------------------------------
# Fixtures: fake tools
# ---------------------------------------------------------------------------


class EchoInput(BaseModel):
    message: str = Field(description="Message to echo")


class EchoTool(BaseTool):
    """Echo the input message back.

    Returns:
        echoed (str): The echoed message
    """

    name = "echo"
    description = "Echo a message."
    input_model = EchoInput

    async def execute(self, arguments: EchoInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(output=json.dumps({"echoed": arguments.message}))


class AddInput(BaseModel):
    a: int = Field(description="First number")
    b: int = Field(description="Second number")


class AddTool(BaseTool):
    """Add two numbers.

    Returns:
        result (int): The sum
    """

    name = "add"
    description = "Add two numbers."
    input_model = AddInput

    async def execute(self, arguments: AddInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(output=json.dumps({"result": arguments.a + arguments.b}))


class LoadSkillReferenceInput(BaseModel):
    skill_name: str = Field(description="Skill slug")
    reference_name: str = Field(description="Reference name")


class LoadSkillReferenceTool(BaseTool):
    name = "load_skill_reference"
    description = "Dummy skill reference loader."
    input_model = LoadSkillReferenceInput

    def __init__(self) -> None:
        self.calls = 0

    async def execute(
        self,
        arguments: LoadSkillReferenceInput,
        context: ToolExecutionContext,
    ) -> ToolResult:
        self.calls += 1
        if (
            arguments.skill_name == "team-planner-playbook"
            and arguments.reference_name == "plan-json-contract"
        ):
            context.metadata["_required_next_tool"] = {
                "tool_name": "submit_plan",
                "reason": "plan-json-contract is active.",
                "reset_hint": "Reload the ending chain if needed.",
            }
        return ToolResult(output="loaded")


class SubmitPlanInput(BaseModel):
    tasks: list[dict[str, object]] = Field(default_factory=list)
    rationale: str = Field(default="")


class SubmitPlanTool(BaseTool):
    name = "submit_plan"
    description = "Dummy submit plan tool."
    input_model = SubmitPlanInput

    def __init__(self) -> None:
        self.calls = 0

    async def execute(self, arguments: SubmitPlanInput, context: ToolExecutionContext) -> ToolResult:
        del arguments, context
        self.calls += 1
        return ToolResult(output="submitted")


class NoDocTool(BaseTool):
    """A tool with no Returns section."""

    name = "no_doc"
    description = "No output schema."
    input_model = EchoInput

    async def execute(self, arguments: EchoInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(output="plain text")


class FailingTool(BaseTool):
    """A tool that always fails."""

    name = "failing"
    description = "Always fails."
    input_model = EchoInput

    async def execute(self, arguments: EchoInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(output="something went wrong", is_error=True)


class SpyRunSubagentInput(BaseModel):
    agent_name: str = Field(description="Subagent name")
    input: dict = Field(description="Structured payload")


class SpyRunSubagentTool(BaseTool):
    """Background tool that reports what scout traces are visible during execution."""

    name = "run_subagent"
    description = "Spy background subagent launcher."
    input_model = SpyRunSubagentInput
    background = "always"

    async def execute(
        self, arguments: SpyRunSubagentInput, context: ToolExecutionContext
    ) -> ToolResult:
        del arguments
        seen = list(context.metadata.get("_scout_target_paths_this_turn", []))
        return ToolResult(output=json.dumps({"seen_paths": seen}), is_error=False)


def _make_toolkit(*tools: BaseTool) -> BaseToolkit:
    return BaseToolkit(name="test_toolkit", description="Test", tools=list(tools))


def _make_registry(*tools: BaseTool) -> ToolRegistry:
    registry = ToolRegistry()
    registry.register_toolkit(_make_toolkit(*tools))
    return registry


# ---------------------------------------------------------------------------
# Fake API client
# ---------------------------------------------------------------------------


class FakeApiClient:
    """Returns pre-configured responses sequentially."""

    def __init__(self, responses: list[ConversationMessage]) -> None:
        self._responses = list(responses)

    async def stream_message(self, request: ApiMessageRequest):
        msg = self._responses.pop(0)
        for block in msg.content:
            if isinstance(block, TextBlock) and block.text:
                yield ApiTextDeltaEvent(text=block.text)
        yield ApiMessageCompleteEvent(
            message=msg,
            usage=UsageSnapshot(input_tokens=1, output_tokens=1),
            stop_reason=None,
        )


class FakeStreamingApiClient:
    """Returns pre-configured streaming event batches sequentially."""

    def __init__(self, event_batches: list[list[object]]) -> None:
        self._event_batches = list(event_batches)

    async def stream_message(self, request: ApiMessageRequest):
        del request
        batch = self._event_batches.pop(0)
        for event in batch:
            yield event


# ---------------------------------------------------------------------------
# Helpers: reduce per-test boilerplate
# ---------------------------------------------------------------------------


def _make_context(
    client,
    registry: ToolRegistry,
    tmp_path: Path,
    **kwargs,
) -> QueryContext:
    return QueryContext(
        api_client=client,
        tool_registry=registry,
        cwd=tmp_path,
        model="test",
        system_prompt="test",
        max_tokens=100,
        **kwargs,
    )


def _tool_reply(*tool_uses: ToolUseBlock) -> ConversationMessage:
    return ConversationMessage(role="assistant", content=list(tool_uses))


def _text_reply(text: str = "ok") -> ConversationMessage:
    return ConversationMessage(role="assistant", content=[TextBlock(text=text)])


async def _collect_events(context: QueryContext, user_text: str) -> list:
    messages = [ConversationMessage.from_user_text(user_text)]
    events = []
    _messages, event_stream = await run_query(context, messages)
    async for event, _usage in event_stream:
        events.append(event)
    return events


@pytest.mark.asyncio
async def test_execute_tool_call_rejects_wrong_tool_when_terminal_guard_active(tmp_path: Path):
    registry = _make_registry(EchoTool())
    client = FakeApiClient([_text_reply()])
    metadata = ExecutionMetadata()
    metadata["_required_next_tool"] = {
        "tool_name": "submit_plan",
        "reason": "plan-json-contract is active.",
        "reset_hint": "Reload the ending chain if needed.",
    }
    context = _make_context(client, registry, tmp_path, tool_metadata=metadata)

    result = await _execute_tool_call(
        context,
        "echo",
        "tool-1",
        {"message": "hi"},
    )

    assert result.is_error is True
    assert "submit_plan" in result.content
    assert context.tool_metadata is not None
    assert context.tool_metadata.get("_required_next_tool") is not None


@pytest.mark.asyncio
async def test_execute_tool_call_clears_terminal_guard_on_expected_tool(tmp_path: Path):
    registry = _make_registry(EchoTool())
    client = FakeApiClient([_text_reply()])
    metadata = ExecutionMetadata()
    metadata["_required_next_tool"] = {
        "tool_name": "echo",
        "reason": "terminal echo required.",
    }
    context = _make_context(client, registry, tmp_path, tool_metadata=metadata)

    result = await _execute_tool_call(
        context,
        "echo",
        "tool-1",
        {"message": "hi"},
    )

    assert result.is_error is False
    assert context.tool_metadata is not None
    assert context.tool_metadata.get("_required_next_tool") is None


# ---------------------------------------------------------------------------
# Tests: tool registration
# ---------------------------------------------------------------------------


class TestToolRegistration:
    def test_register_tool(self):
        registry = _make_registry(EchoTool())
        assert registry.get("echo") is not None
        assert registry.get("nonexistent") is None

    def test_register_toolkit(self):
        registry = _make_registry(EchoTool(), AddTool())
        assert len(registry.list_tools()) == 2
        assert len(registry.list_toolkits()) == 1

    def test_restrict_to_toolkits(self):
        registry = ToolRegistry()
        tk1 = BaseToolkit(name="tk1", description="A", tools=[EchoTool()])
        tk2 = BaseToolkit(name="tk2", description="B", tools=[AddTool()])
        registry.register_toolkit(tk1)
        registry.register_toolkit(tk2)
        assert len(registry.list_tools()) == 2

        registry.restrict_to_toolkits(["tk1"])
        assert len(registry.list_tools()) == 1
        assert registry.get("echo") is not None
        assert registry.get("add") is None


# ---------------------------------------------------------------------------
# Tests: output schema from docstrings
# ---------------------------------------------------------------------------


class TestOutputSchema:
    def test_output_schema_from_docstring(self):
        tool = EchoTool()
        schema = tool.output_schema()
        assert schema is not None
        assert schema["type"] == "object"
        assert "echoed" in schema["properties"]
        assert schema["properties"]["echoed"]["type"] == "string"

    def test_output_schema_with_int_type(self):
        tool = AddTool()
        schema = tool.output_schema()
        assert schema is not None
        assert "result" in schema["properties"]
        assert schema["properties"]["result"]["type"] == "integer"

    def test_no_output_schema_without_returns(self):
        tool = NoDocTool()
        assert tool.output_schema() is None

    def test_api_schema_includes_output(self):
        tool = EchoTool()
        api = tool.to_api_schema()
        assert "output_schema" in api
        assert api["output_schema"]["properties"]["echoed"]["type"] == "string"

    def test_api_schema_omits_output_when_none(self):
        tool = NoDocTool()
        api = tool.to_api_schema()
        assert "output_schema" not in api

    def test_api_schema_includes_input(self):
        tool = EchoTool()
        api = tool.to_api_schema()
        assert "input_schema" in api
        assert "message" in api["input_schema"]["properties"]


# ---------------------------------------------------------------------------
# Tests: tool call loop (query.py)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_single_tool_call(tmp_path: Path):
    """Model calls a tool, gets result, then responds with text."""
    registry = _make_registry(EchoTool())
    client = FakeApiClient([
        _tool_reply(ToolUseBlock(id="tc1", name="echo", input={"message": "hello"})),
        _text_reply("Done."),
    ])
    context = _make_context(client, registry, tmp_path)
    events = await _collect_events(context, "echo hello")

    tool_starts = [e for e in events if isinstance(e, ToolExecutionStarted)]
    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert len(tool_starts) == 1
    assert tool_starts[0].tool_name == "echo"
    assert len(tool_completes) == 1
    assert not tool_completes[0].is_error
    parsed = json.loads(tool_completes[0].output)
    assert parsed["echoed"] == "hello"


@pytest.mark.asyncio
async def test_background_scout_trace_is_recorded_after_launch_not_before(tmp_path: Path):
    registry = _make_registry(SpyRunSubagentTool())
    context = _make_context(
        FakeApiClient([]),
        registry,
        tmp_path,
        tool_metadata=ExecutionMetadata(),
    )
    manager = BackgroundTaskManager()
    tool_use = ToolUseBlock(
        id="tc1",
        name="run_subagent",
        input={
            "agent_name": "scout",
            "input": {"target_paths": ["/testbed/pydantic/json_schema.py"]},
        },
    )

    result, started, rejected = _launch_background_tool(
        context,
        manager,
        tool_use,
        task_note="launch scout",
    )

    assert not result.is_error
    assert started is not None
    assert rejected is None
    assert "Keep using the current turn on other ready work first" in result.content
    assert "do not wait immediately unless this task is the only blocker left" in result.content

    completed = await manager.wait_any(timeout=1.0)
    assert completed is not None
    assert completed.result is not None
    payload = json.loads(completed.result.output)
    assert payload["seen_paths"] == ["/testbed/pydantic/json_schema.py"]
    assert context.tool_metadata is not None
    assert context.tool_metadata["_scout_launches_this_turn"] == 1
    assert context.tool_metadata["_scout_target_paths_this_turn"] == [
        "/testbed/pydantic/json_schema.py"
    ]


@pytest.mark.asyncio
async def test_unknown_tool_returns_error(tmp_path: Path):
    """Model calls a tool that doesn't exist — should get an error result."""
    registry = _make_registry(EchoTool())
    client = FakeApiClient([
        _tool_reply(ToolUseBlock(id="tc1", name="nonexistent_tool", input={})),
        _text_reply("ok"),
    ])
    context = _make_context(client, registry, tmp_path)
    events = await _collect_events(context, "do something")

    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]
    assert len(tool_completes) == 1
    assert tool_completes[0].is_error
    assert "Unknown tool" in tool_completes[0].output


@pytest.mark.asyncio
async def test_invalid_input_returns_error(tmp_path: Path):
    """Model passes invalid input to a tool — should get a validation error."""
    registry = _make_registry(AddTool())
    client = FakeApiClient([
        _tool_reply(ToolUseBlock(id="tc1", name="add", input={"a": "not_a_number", "b": 2})),
        _text_reply("ok"),
    ])
    context = _make_context(client, registry, tmp_path)
    events = await _collect_events(context, "add")

    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]
    assert len(tool_completes) == 1
    assert tool_completes[0].is_error
    assert "Invalid input" in tool_completes[0].output


@pytest.mark.asyncio
async def test_tool_error_propagated(tmp_path: Path):
    """Tool returns is_error=True — should be reflected in events."""
    registry = _make_registry(FailingTool())
    client = FakeApiClient([
        _tool_reply(ToolUseBlock(id="tc1", name="failing", input={"message": "x"})),
        _text_reply("ok"),
    ])
    context = _make_context(client, registry, tmp_path)
    events = await _collect_events(context, "fail")

    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]
    assert len(tool_completes) == 1
    assert tool_completes[0].is_error
    assert "something went wrong" in tool_completes[0].output


@pytest.mark.asyncio
async def test_parallel_tool_calls(tmp_path: Path):
    """Model calls multiple tools in one turn — should execute in parallel."""
    registry = _make_registry(EchoTool(), AddTool())
    client = FakeApiClient([
        _tool_reply(
            ToolUseBlock(id="tc1", name="echo", input={"message": "hi"}),
            ToolUseBlock(id="tc2", name="add", input={"a": 3, "b": 4}),
        ),
        _text_reply("Both done."),
    ])
    context = _make_context(client, registry, tmp_path)
    events = await _collect_events(context, "do both")

    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]
    assert len(tool_completes) == 2
    outputs = [json.loads(tc.output) for tc in tool_completes if not tc.is_error]
    # Check both tools returned correct results (order may vary)
    assert any(o.get("echoed") == "hi" for o in outputs)
    assert any(o.get("result") == 7 for o in outputs)


@pytest.mark.asyncio
async def test_parallel_batch_rejects_terminal_reference_with_sibling_tool(tmp_path: Path):
    load_ref = LoadSkillReferenceTool()
    echo = EchoTool()
    registry = _make_registry(load_ref, echo)
    client = FakeApiClient([
        _tool_reply(
            ToolUseBlock(
                id="tc1",
                name="load_skill_reference",
                input={
                    "skill_name": "team-planner-playbook",
                    "reference_name": "plan-json-contract",
                },
            ),
            ToolUseBlock(id="tc2", name="echo", input={"message": "hi"}),
        ),
        _text_reply("Recovered."),
    ])
    context = _make_context(client, registry, tmp_path)

    events = await _collect_events(context, "finalize the root plan")

    tool_starts = [e for e in events if isinstance(e, ToolExecutionStarted)]
    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert tool_starts == []
    assert len(tool_completes) == 2
    assert all(event.is_error for event in tool_completes)
    assert all("must be loaded alone" in event.output for event in tool_completes)
    assert load_ref.calls == 0


@pytest.mark.asyncio
async def test_parallel_batch_rejects_sibling_tool_when_terminal_guard_is_active(tmp_path: Path):
    submit_plan = SubmitPlanTool()
    echo = EchoTool()
    registry = _make_registry(submit_plan, echo)
    client = FakeApiClient([
        _tool_reply(
            ToolUseBlock(
                id="tc1",
                name="submit_plan",
                input={"tasks": [], "rationale": "ready"},
            ),
            ToolUseBlock(id="tc2", name="echo", input={"message": "extra"}),
        ),
        _text_reply("Recovered."),
    ])
    metadata = ExecutionMetadata()
    metadata["_required_next_tool"] = {
        "tool_name": "submit_plan",
        "reason": "plan-json-contract is active.",
        "reset_hint": "Reload the ending chain if needed.",
    }
    context = _make_context(client, registry, tmp_path, tool_metadata=metadata)

    events = await _collect_events(context, "submit the plan")

    tool_starts = [e for e in events if isinstance(e, ToolExecutionStarted)]
    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert tool_starts == []
    assert len(tool_completes) == 2
    assert all(event.is_error for event in tool_completes)
    assert all("Submit only `submit_plan(...)` in the next tool batch." in event.output for event in tool_completes)
    assert submit_plan.calls == 0


@pytest.mark.asyncio
async def test_terminal_reference_guard_persists_across_turns(tmp_path: Path):
    load_ref = LoadSkillReferenceTool()
    echo = EchoTool()
    registry = _make_registry(load_ref, echo)
    client = FakeApiClient([
        _tool_reply(
            ToolUseBlock(
                id="tc1",
                name="load_skill_reference",
                input={
                    "skill_name": "team-planner-playbook",
                    "reference_name": "plan-json-contract",
                },
            )
        ),
        _tool_reply(ToolUseBlock(id="tc2", name="echo", input={"message": "wrong next tool"})),
        _text_reply("Recovered."),
    ])
    context = _make_context(client, registry, tmp_path)

    events = await _collect_events(context, "load the plan contract, then do the wrong thing")

    tool_starts = [e for e in events if isinstance(e, ToolExecutionStarted)]
    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert len(tool_starts) == 1
    assert tool_starts[0].tool_name == "load_skill_reference"
    assert len(tool_completes) == 2
    assert tool_completes[0].is_error is False
    assert tool_completes[1].is_error is True
    assert "Submit only `submit_plan(...)` in the next tool batch." in tool_completes[1].output
    assert load_ref.calls == 1


@pytest.mark.asyncio
async def test_streaming_terminal_reference_batch_rejects_sibling_tool(tmp_path: Path):
    load_ref = LoadSkillReferenceTool()
    echo = EchoTool()
    registry = _make_registry(load_ref, echo)
    client = FakeStreamingApiClient(
        [
            [
                ApiToolUseDeltaEvent(
                    id="tc1",
                    name="load_skill_reference",
                    input={
                        "skill_name": "team-planner-playbook",
                        "reference_name": "plan-json-contract",
                    },
                ),
                ApiToolUseDeltaEvent(id="tc2", name="echo", input={"message": "hi"}),
                ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[
                            ToolUseBlock(
                                id="tc1",
                                name="load_skill_reference",
                                input={
                                    "skill_name": "team-planner-playbook",
                                    "reference_name": "plan-json-contract",
                                },
                            ),
                            ToolUseBlock(id="tc2", name="echo", input={"message": "hi"}),
                        ],
                    ),
                    usage=UsageSnapshot(input_tokens=1, output_tokens=1),
                    stop_reason=None,
                ),
            ],
            [
                ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[TextBlock(text="Recovered.")],
                    ),
                    usage=UsageSnapshot(input_tokens=1, output_tokens=1),
                    stop_reason=None,
                ),
            ],
        ]
    )
    context = _make_context(client, registry, tmp_path)

    events = await _collect_events(context, "load the plan contract and batch another tool")

    tool_starts = [e for e in events if isinstance(e, ToolExecutionStarted)]
    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert tool_starts == []
    assert len(tool_completes) == 2
    assert all(event.is_error for event in tool_completes)
    assert all("must be loaded alone" in event.output for event in tool_completes)
    assert load_ref.calls == 0


@pytest.mark.asyncio
async def test_streaming_pending_terminal_guard_rejects_wrong_tool_without_starting_it(
    tmp_path: Path,
):
    registry = _make_registry(EchoTool())
    client = FakeStreamingApiClient(
        [
            [
                ApiToolUseDeltaEvent(id="tc1", name="echo", input={"message": "wrong next tool"}),
                ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[
                            ToolUseBlock(
                                id="tc1",
                                name="echo",
                                input={"message": "wrong next tool"},
                            )
                        ],
                    ),
                    usage=UsageSnapshot(input_tokens=1, output_tokens=1),
                    stop_reason=None,
                ),
            ],
            [
                ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[TextBlock(text="Recovered.")],
                    ),
                    usage=UsageSnapshot(input_tokens=1, output_tokens=1),
                    stop_reason=None,
                ),
            ],
        ]
    )
    metadata = ExecutionMetadata()
    metadata["_required_next_tool"] = {
        "tool_name": "submit_plan",
        "reason": "plan-json-contract is active.",
        "reset_hint": "Reload the ending chain if needed.",
    }
    context = _make_context(client, registry, tmp_path, tool_metadata=metadata)

    events = await _collect_events(context, "do the wrong tool under a terminal guard")

    tool_starts = [e for e in events if isinstance(e, ToolExecutionStarted)]
    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert tool_starts == []
    assert len(tool_completes) == 1
    assert tool_completes[0].is_error is True
    assert "Submit only `submit_plan(...)` in the next tool batch." in tool_completes[0].output


@pytest.mark.asyncio
async def test_streaming_tool_calls_respect_planner_soft_limit(tmp_path: Path):
    registry = _make_registry(EchoTool())
    client = FakeStreamingApiClient(
        [
            [
                ApiToolUseDeltaEvent(id="tc1", name="echo", input={"message": "first"}),
                ApiToolUseDeltaEvent(id="tc2", name="echo", input={"message": "second"}),
                ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[
                            ToolUseBlock(id="tc1", name="echo", input={"message": "first"}),
                            ToolUseBlock(id="tc2", name="echo", input={"message": "second"}),
                        ],
                    ),
                    usage=UsageSnapshot(input_tokens=1, output_tokens=1),
                    stop_reason=None,
                ),
            ],
            [
                ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[TextBlock(text="Done.")],
                    ),
                    usage=UsageSnapshot(input_tokens=1, output_tokens=1),
                    stop_reason=None,
                ),
            ],
        ]
    )
    context = _make_context(client, registry, tmp_path, tool_call_limit=1)
    events = await _collect_events(context, "echo twice")

    tool_starts = [e for e in events if isinstance(e, ToolExecutionStarted)]
    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert len(tool_starts) == 1
    assert tool_starts[0].tool_name == "echo"
    assert any(not event.is_error and '"echoed": "first"' in event.output for event in tool_completes)
    assert any(event.is_error and "tool_call_limit exceeded" in event.output for event in tool_completes)


@pytest.mark.asyncio
async def test_no_tool_calls_returns_immediately(tmp_path: Path):
    """Model responds with text only — loop should end after one turn."""
    registry = _make_registry(EchoTool())
    client = FakeApiClient([_text_reply("Just text.")])
    context = _make_context(client, registry, tmp_path)
    events = await _collect_events(context, "hello")

    turns = [e for e in events if isinstance(e, AssistantTurnComplete)]
    assert len(turns) == 1
    assert turns[0].message.text == "Just text."
    # No tool events
    assert not any(isinstance(e, ToolExecutionStarted) for e in events)


# ---------------------------------------------------------------------------
# Tests: Daytona tools output schema
# ---------------------------------------------------------------------------


class TestDaytonaToolSchemas:
    def test_all_daytona_tools_have_output_schemas(self):
        from tools.daytona_toolkit.toolkit import DaytonaToolkit

        tk = DaytonaToolkit(sandbox_id="test")
        for tool in tk.list_tools():
            schema = tool.to_api_schema()
            assert "output_schema" in schema, (
                f"{tool.name} missing output_schema — add Returns: to docstring"
            )
            assert "properties" in schema["output_schema"]
