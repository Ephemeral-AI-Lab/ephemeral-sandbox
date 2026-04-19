"""Tests for tool registration, schema generation, and the tool call loop."""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from pydantic import BaseModel, ConfigDict, Field

from message import ConversationMessage, TextBlock, ToolResultBlock, ToolUseBlock
from engine.core.query import QueryContext, QueryExitReason, _execute_tool_call, run_query
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
    decorate_schemas_for_background,
    run_tool_safely,
)
from tools.core.decorator import tool


# ---------------------------------------------------------------------------
# Fixtures: fake tools
# ---------------------------------------------------------------------------


class EchoInput(BaseModel):
    message: str = Field(description="Message to echo")


class EchoOutput(BaseModel):
    echoed: str = Field(description="The echoed message")


class StrictEchoInput(BaseModel):
    model_config = ConfigDict(extra="forbid")

    message: str = Field(description="Message to echo")


class EchoTool(BaseTool):
    name = "echo"
    description = "Echo a message."
    input_model = EchoInput
    output_model = EchoOutput

    async def execute(self, arguments: EchoInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(output=json.dumps({"echoed": arguments.message}))


class StrictEchoTool(BaseTool):
    name = "strict_echo"
    description = "Echo a message with strict input validation."
    input_model = StrictEchoInput
    output_model = EchoOutput

    async def execute(self, arguments: StrictEchoInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(output=json.dumps({"echoed": arguments.message}))


class AddInput(BaseModel):
    a: int = Field(description="First number")
    b: int = Field(description="Second number")


class AddOutput(BaseModel):
    result: int = Field(description="The sum")


class AddTool(BaseTool):
    name = "add"
    description = "Add two numbers."
    input_model = AddInput
    output_model = AddOutput

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
        return ToolResult(output="loaded")


class ExplicitDecoratorInput(BaseModel):
    value: str = Field(description="Explicit input value")


class ExplicitDecoratorOutput(BaseModel):
    value: str = Field(description="Explicit output value")


@tool(
    name="explicit_decorator_tool",
    description="Tool with explicit Pydantic schemas.",
    input_model=ExplicitDecoratorInput,
    output_model=ExplicitDecoratorOutput,
)
async def explicit_decorator_tool(
    value: str,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    del context
    return ToolResult(output=json.dumps({"value": value}))


class SubmitPlanInput(BaseModel):
    new_tasks: list[dict[str, object]] = Field(default_factory=list)


class SubmitPlanTool(BaseTool):
    name = "submit_plan"
    description = "Dummy submit plan tool."
    input_model = SubmitPlanInput

    def __init__(self) -> None:
        self.calls = 0

    async def execute(
        self, arguments: SubmitPlanInput, context: ToolExecutionContext
    ) -> ToolResult:
        del arguments, context
        self.calls += 1
        return ToolResult(output="submitted")


class RejectingSubmitPlanTool(BaseTool):
    name = "submit_plan"
    description = "Dummy submit plan tool that rejects the payload."
    input_model = SubmitPlanInput

    async def execute(
        self, arguments: SubmitPlanInput, context: ToolExecutionContext
    ) -> ToolResult:
        del arguments, context
        return ToolResult(output="validation failed", is_error=True)


class PlainTextTool(BaseTool):
    """A tool that relies on the default plain-text output schema."""

    name = "plain_text"
    description = "Plain text output."
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


class MetadataTool(BaseTool):
    """A tool that returns metadata used by downstream display/API reducers."""

    name = "metadata_tool"
    description = "Returns metadata."
    input_model = EchoInput

    async def execute(self, arguments: EchoInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(
            output=arguments.message,
            metadata={"trace": {"message": arguments.message}},
        )


class InvalidJsonOutputTool(BaseTool):
    name = "invalid_json_output"
    description = "Returns non-JSON despite declaring a structured output."
    input_model = EchoInput
    output_model = EchoOutput

    async def execute(self, arguments: EchoInput, context: ToolExecutionContext) -> ToolResult:
        return ToolResult(output="plain text")


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
async def test_execute_tool_call_ignores_stale_next_tool_guard_metadata(tmp_path: Path):
    registry = _make_registry(EchoTool())
    client = FakeApiClient([_text_reply()])
    metadata = ExecutionMetadata()
    metadata["_required_next_tool"] = {
        "tool_name": "submit_plan",
        "reason": "plan-json-contract is active.",
        "reset_hint": "Reload the ending chain if needed.",
    }
    context = _make_context(
        client,
        registry,
        tmp_path,
        tool_metadata=metadata,
        terminal_tools={"submit_plan"},
    )

    result = await _execute_tool_call(
        context,
        "echo",
        "tool-1",
        {"message": "hi"},
    )

    assert result.is_error is False
    assert json.loads(result.content) == {"echoed": "hi"}
    assert context.tool_metadata is not None
    assert context.tool_metadata.get("_required_next_tool") is not None


@pytest.mark.asyncio
async def test_execute_tool_call_does_not_require_expected_next_tool(tmp_path: Path):
    registry = _make_registry(EchoTool())
    client = FakeApiClient([_text_reply()])
    metadata = ExecutionMetadata()
    metadata["_required_next_tool"] = {
        "tool_name": "echo",
        "reason": "terminal echo required.",
    }
    context = _make_context(
        client,
        registry,
        tmp_path,
        tool_metadata=metadata,
        terminal_tools={"submit_plan"},
    )

    result = await _execute_tool_call(
        context,
        "echo",
        "tool-1",
        {"message": "hi"},
    )

    assert result.is_error is False
    assert json.loads(result.content) == {"echoed": "hi"}
    assert context.tool_metadata is not None
    assert context.tool_metadata.get("_required_next_tool") is not None


@pytest.mark.asyncio
async def test_execute_tool_call_strips_runtime_control_fields_for_foreground_tools(
    tmp_path: Path,
):
    registry = _make_registry(StrictEchoTool())
    client = FakeApiClient([_text_reply()])
    context = _make_context(client, registry, tmp_path)

    result = await _execute_tool_call(
        context,
        "strict_echo",
        "tool-1",
        {
            "message": "hi",
            "task_note": "model-facing background note",
            "background": False,
        },
    )

    assert result.is_error is False
    assert json.loads(result.content) == {"echoed": "hi"}


def test_background_schema_decorator_skips_terminal_tools():
    registry = _make_registry(EchoTool(), SubmitPlanTool())
    schemas = decorate_schemas_for_background(
        registry,
        registry.to_api_schema(),
        terminal_tools={"submit_plan"},
    )

    by_name = {schema["name"]: schema for schema in schemas}
    echo_schema = by_name["echo"]["input_schema"]
    submit_schema = by_name["submit_plan"]["input_schema"]

    assert "task_note" in echo_schema["properties"]
    assert "task_note" in echo_schema["required"]
    assert "task_note" not in submit_schema["properties"]
    assert "task_note" not in submit_schema.get("required", [])
    assert "background" not in submit_schema["properties"]


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
# Tests: output schema from Pydantic models
# ---------------------------------------------------------------------------


class TestOutputSchema:
    def test_output_schema_from_output_model(self):
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

    def test_default_output_schema_is_plain_text(self):
        tool = PlainTextTool()
        schema = tool.output_schema()
        assert schema["type"] == "string"
        assert schema["description"] == (
            "Successful output for tools whose true output is plain text."
        )

    def test_api_schema_includes_output(self):
        tool = EchoTool()
        api = tool.to_api_schema()
        assert "output_schema" in api
        assert api["output_schema"]["properties"]["echoed"]["type"] == "string"

    def test_api_schema_includes_default_text_output(self):
        tool = PlainTextTool()
        api = tool.to_api_schema()
        assert api["output_schema"]["type"] == "string"

    def test_decorator_uses_explicit_pydantic_models(self):
        api = explicit_decorator_tool.to_api_schema()
        assert api["input_schema"]["properties"]["value"]["description"] == (
            "Explicit input value"
        )
        assert api["output_schema"]["properties"]["value"]["description"] == (
            "Explicit output value"
        )

    def test_api_schema_includes_input(self):
        tool = EchoTool()
        api = tool.to_api_schema()
        assert "input_schema" in api
        assert "message" in api["input_schema"]["properties"]

    @pytest.mark.asyncio
    async def test_run_tool_safely_validates_structured_success_output(self):
        result = await run_tool_safely(
            InvalidJsonOutputTool(),
            {"message": "hello"},
            ToolExecutionContext(cwd=Path("/tmp")),
        )

        assert result.is_error is True
        assert "Invalid output from invalid_json_output" in result.output
        assert "expected JSON matching EchoOutput" in result.output

    @pytest.mark.asyncio
    async def test_run_tool_safely_accepts_default_text_output(self):
        result = await run_tool_safely(
            PlainTextTool(),
            {"message": "hello"},
            ToolExecutionContext(cwd=Path("/tmp")),
        )

        assert result.is_error is False
        assert result.output == "plain text"


# ---------------------------------------------------------------------------
# Tests: tool call loop (query.py)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_single_tool_call(tmp_path: Path):
    """Model calls a tool, gets result, then responds with text."""
    registry = _make_registry(EchoTool())
    client = FakeApiClient(
        [
            _tool_reply(ToolUseBlock(id="tc1", name="echo", input={"message": "hello"})),
            _text_reply("Done."),
        ]
    )
    context = _make_context(client, registry, tmp_path, enable_background_tasks=True)
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
async def test_unknown_tool_returns_error(tmp_path: Path):
    """Model calls a tool that doesn't exist — should get an error result."""
    registry = _make_registry(EchoTool())
    client = FakeApiClient(
        [
            _tool_reply(ToolUseBlock(id="tc1", name="nonexistent_tool", input={})),
            _text_reply("ok"),
        ]
    )
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
    client = FakeApiClient(
        [
            _tool_reply(ToolUseBlock(id="tc1", name="add", input={"a": "not_a_number", "b": 2})),
            _text_reply("ok"),
        ]
    )
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
    client = FakeApiClient(
        [
            _tool_reply(ToolUseBlock(id="tc1", name="failing", input={"message": "x"})),
            _text_reply("ok"),
        ]
    )
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
    client = FakeApiClient(
        [
            _tool_reply(
                ToolUseBlock(id="tc1", name="echo", input={"message": "hi"}),
                ToolUseBlock(id="tc2", name="add", input={"a": 3, "b": 4}),
            ),
            _text_reply("Both done."),
        ]
    )
    context = _make_context(client, registry, tmp_path)
    events = await _collect_events(context, "do both")

    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]
    assert len(tool_completes) == 2
    outputs = [json.loads(tc.output) for tc in tool_completes if not tc.is_error]
    # Check both tools returned correct results (order may vary)
    assert any(o.get("echoed") == "hi" for o in outputs)
    assert any(o.get("result") == 7 for o in outputs)


@pytest.mark.asyncio
async def test_parallel_batch_still_rejects_terminal_tool_with_sibling(tmp_path: Path):
    submit_plan = SubmitPlanTool()
    echo = EchoTool()
    registry = _make_registry(submit_plan, echo)
    client = FakeApiClient(
        [
            _tool_reply(
                ToolUseBlock(
                    id="tc1",
                    name="submit_plan",
                    input={"new_tasks": []},
                ),
                ToolUseBlock(id="tc2", name="echo", input={"message": "extra"}),
            ),
            _text_reply("Recovered."),
            _text_reply("still thinking"),
            _text_reply("still thinking"),
            _text_reply("still thinking"),
        ]
    )
    metadata = ExecutionMetadata()
    metadata["_required_next_tool"] = {
        "tool_name": "submit_plan",
        "reason": "plan-json-contract is active.",
        "reset_hint": "Reload the ending chain if needed.",
    }
    context = _make_context(
        client,
        registry,
        tmp_path,
        tool_metadata=metadata,
        terminal_tools={"submit_plan"},
    )

    events = await _collect_events(context, "submit the plan")

    tool_starts = [e for e in events if isinstance(e, ToolExecutionStarted)]
    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert tool_starts == []
    assert len(tool_completes) == 2
    assert all(event.is_error for event in tool_completes)
    assert all(
        "Terminal tool `submit_plan` must be called alone." in event.output
        for event in tool_completes
    )
    assert submit_plan.calls == 0


@pytest.mark.asyncio
async def test_failed_terminal_tool_call_does_not_stop_query_loop(tmp_path: Path):
    registry = _make_registry(RejectingSubmitPlanTool())
    client = FakeApiClient(
        [
            _tool_reply(
                ToolUseBlock(
                    id="tc1",
                    name="submit_plan",
                    input={"new_tasks": []},
                )
            ),
            _text_reply("I will repair the payload."),
            _text_reply("still thinking"),
            _text_reply("still thinking"),
            _text_reply("still thinking"),
        ]
    )
    context = _make_context(
        client,
        registry,
        tmp_path,
        terminal_tools={"submit_plan"},
    )

    events = await _collect_events(context, "submit the plan")

    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert len(tool_completes) == 1
    assert tool_completes[0].tool_name == "submit_plan"
    assert tool_completes[0].is_error is True
    assert context.exit_reason == QueryExitReason.TEXT_RESPONSE
    assert context.terminal_nudge_retries_used == 3


@pytest.mark.asyncio
async def test_terminal_nudge_recovers_with_tool_call(tmp_path: Path):
    submit_plan = SubmitPlanTool()
    registry = _make_registry(submit_plan)
    client = FakeApiClient(
        [
            _text_reply("Here is my plan in prose."),
            _tool_reply(
                ToolUseBlock(
                    id="tc1",
                    name="submit_plan",
                    input={"new_tasks": []},
                )
            ),
        ]
    )
    context = _make_context(
        client,
        registry,
        tmp_path,
        terminal_tools={"submit_plan"},
    )

    events = await _collect_events(context, "submit the plan")

    assert context.exit_reason == QueryExitReason.TOOL_STOP
    assert submit_plan.calls == 1
    assert context.terminal_nudge_retries_used == 1
    turns = [e for e in events if isinstance(e, AssistantTurnComplete)]
    assert len(turns) == 2


@pytest.mark.asyncio
async def test_terminal_nudge_extends_tool_budget_once(tmp_path: Path):
    submit_plan = SubmitPlanTool()
    registry = _make_registry(submit_plan)
    client = FakeApiClient(
        [
            _text_reply("narration 1"),
            _text_reply("narration 2"),
            _tool_reply(
                ToolUseBlock(
                    id="tc1",
                    name="submit_plan",
                    input={"new_tasks": []},
                )
            ),
        ]
    )
    context = _make_context(
        client,
        registry,
        tmp_path,
        terminal_tools={"submit_plan"},
        tool_call_limit=1,
    )

    await _collect_events(context, "submit the plan")

    assert context.exit_reason == QueryExitReason.TOOL_STOP
    assert context.terminal_nudge_retries_used == 2
    assert context.terminal_nudge_budget_extended is True
    assert context.tool_call_limit == 11


@pytest.mark.asyncio
async def test_terminal_nudge_capped_at_three_retries(tmp_path: Path):
    registry = _make_registry(SubmitPlanTool())
    client = FakeApiClient(
        [_text_reply("text only") for _ in range(5)]
    )
    context = _make_context(
        client,
        registry,
        tmp_path,
        terminal_tools={"submit_plan"},
    )

    await _collect_events(context, "submit the plan")

    assert context.exit_reason == QueryExitReason.TEXT_RESPONSE
    assert context.terminal_nudge_retries_used == 3


@pytest.mark.asyncio
async def test_successful_terminal_tool_call_stops_query_loop(tmp_path: Path):
    submit_plan = SubmitPlanTool()
    registry = _make_registry(submit_plan)
    client = FakeApiClient(
        [
            _tool_reply(
                ToolUseBlock(
                    id="tc1",
                    name="submit_plan",
                    input={"new_tasks": []},
                )
            ),
            _text_reply("This should not be reached."),
        ]
    )
    context = _make_context(
        client,
        registry,
        tmp_path,
        terminal_tools={"submit_plan"},
    )

    events = await _collect_events(context, "submit the plan")

    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]
    turns = [e for e in events if isinstance(e, AssistantTurnComplete)]

    assert submit_plan.calls == 1
    assert len(tool_completes) == 1
    assert tool_completes[0].is_error is False
    assert len(turns) == 1
    assert context.exit_reason == QueryExitReason.TOOL_STOP


@pytest.mark.asyncio
async def test_exhausted_budget_still_allows_successful_terminal_tool_call(tmp_path: Path):
    submit_plan = SubmitPlanTool()
    registry = _make_registry(submit_plan)
    client = FakeApiClient(
        [
            _tool_reply(
                ToolUseBlock(
                    id="tc1",
                    name="submit_plan",
                    input={"new_tasks": []},
                )
            ),
            _text_reply("This should not be reached."),
        ]
    )
    context = _make_context(
        client,
        registry,
        tmp_path,
        terminal_tools={"submit_plan"},
        tool_call_limit=1,
        tool_calls_used=1,
    )

    events = await _collect_events(context, "submit the plan")

    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert submit_plan.calls == 1
    assert len(tool_completes) == 1
    assert tool_completes[0].is_error is False
    assert context.tool_calls_used == 1
    assert context.exit_reason == QueryExitReason.TOOL_STOP


@pytest.mark.asyncio
async def test_streaming_ignores_stale_next_tool_guard_metadata(
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

    events = await _collect_events(context, "run echo with stale next-tool metadata")

    tool_starts = [e for e in events if isinstance(e, ToolExecutionStarted)]
    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]

    assert len(tool_starts) == 1
    assert len(tool_completes) == 1
    assert tool_completes[0].is_error is False
    assert json.loads(tool_completes[0].output) == {"echoed": "wrong next tool"}


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
    assert any(
        not event.is_error and '"echoed": "first"' in event.output for event in tool_completes
    )
    assert any(
        event.is_error and "tool_call_limit exceeded" in event.output for event in tool_completes
    )


@pytest.mark.asyncio
async def test_streamed_tool_result_preserves_metadata_in_message_history(tmp_path: Path):
    registry = _make_registry(MetadataTool())
    client = FakeStreamingApiClient(
        [
            [
                ApiToolUseDeltaEvent(
                    id="tc1",
                    name="metadata_tool",
                    input={"message": "kept"},
                ),
                ApiMessageCompleteEvent(
                    message=ConversationMessage(
                        role="assistant",
                        content=[
                            ToolUseBlock(
                                id="tc1",
                                name="metadata_tool",
                                input={"message": "kept"},
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
                        content=[TextBlock(text="Done.")],
                    ),
                    usage=UsageSnapshot(input_tokens=1, output_tokens=1),
                    stop_reason=None,
                ),
            ],
        ]
    )
    context = _make_context(client, registry, tmp_path)
    messages = [ConversationMessage.from_user_text("run metadata tool")]
    _messages, event_stream = await run_query(context, messages)

    events = []
    async for event, _usage in event_stream:
        events.append(event)

    tool_completes = [e for e in events if isinstance(e, ToolExecutionCompleted)]
    assert len(tool_completes) == 1
    assert tool_completes[0].metadata == {"trace": {"message": "kept"}}

    tool_result_blocks = [
        block
        for message in messages
        for block in message.content
        if isinstance(block, ToolResultBlock)
    ]
    assert len(tool_result_blocks) == 1
    assert tool_result_blocks[0].metadata == {"trace": {"message": "kept"}}


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
        for daytona_tool in tk.list_tools():
            schema = daytona_tool.to_api_schema()
            assert "output_schema" in schema, (
                f"{daytona_tool.name} missing output_schema — add output_model"
            )
            assert "properties" in schema["output_schema"]
