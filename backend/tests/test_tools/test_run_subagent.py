"""Unit tests for run_subagent and the progress-provider plumbing."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from agents import get_definition as get_agent_definition
from engine.runtime.background_tasks import BackgroundTaskManager
from message.messages import (
    ConversationMessage,
    TextBlock,
    ThinkingBlock,
    ToolResultBlock,
    ToolUseBlock,
)
from providers.types import UsageSnapshot
from tools.core.base import ToolExecutionContext, ToolResult
from tools.subagent.run_subagent_tool import (
    PEEK_MESSAGE_MAX,
    _validate_run_subagent_request,
    format_last_n_messages,
    run_subagent,
)
from team.builtins import register_all as _register_team_builtins


if get_agent_definition("developer") is None:
    try:
        _register_team_builtins()
    except Exception:
        pass


# ---------------------------------------------------------------------------
# format_last_n_messages
# ---------------------------------------------------------------------------


def _make_messages() -> list[ConversationMessage]:
    return [
        ConversationMessage(
            role="user",
            content=[TextBlock(text="please refactor the parser")],
        ),
        ConversationMessage(
            role="assistant",
            content=[
                ThinkingBlock(text="I should read the file first"),
                ToolUseBlock(name="read_file", input={"path": "src/parser.py"}),
            ],
        ),
        ConversationMessage(
            role="user",
            content=[
                ToolResultBlock(
                    tool_use_id="t1", content="def parse(s): return s.split()"
                )
            ],
        ),
        ConversationMessage(
            role="assistant",
            content=[
                TextBlock(text="parser is trivial — adding type hints"),
                ToolUseBlock(
                    name="edit_file",
                    input={"path": "src/parser.py", "old": "def parse(s)", "new": "def parse(s: str) -> list[str]"},
                ),
            ],
        ),
        ConversationMessage(
            role="user",
            content=[ToolResultBlock(tool_use_id="t2", content="OK")],
        ),
    ]


def _touch_rel(repo_root: Path, rel_path: str) -> None:
    path = repo_root / rel_path
    path.parent.mkdir(parents=True, exist_ok=True)
    path.touch()


def test_format_last_n_messages_renders_each_block_type():
    out = format_last_n_messages(_make_messages(), n=PEEK_MESSAGE_MAX)
    assert "[text]" in out
    assert "[think]" in out
    assert "[tool] read_file" in out
    assert "[result]" in out
    # Cap respected (should not blow past total cap).
    assert len(out) <= 2048


def test_format_last_n_messages_empty():
    assert format_last_n_messages([], n=5) == "(no messages yet)"


def test_format_last_n_messages_truncates_long_blocks():
    long_text = "x" * 5000
    msgs = [ConversationMessage(role="assistant", content=[TextBlock(text=long_text)])]
    out = format_last_n_messages(msgs, n=5)
    # The single rendered block must be truncated to ~_PEEK_BLOCK_CHAR_CAP.
    assert len(out) < 500
    assert "…" in out


def test_format_last_n_messages_only_returns_last_n():
    msgs = [
        ConversationMessage(
            role="assistant", content=[TextBlock(text=f"msg-{i}")]
        )
        for i in range(20)
    ]
    out = format_last_n_messages(msgs, n=3)
    assert "msg-19" in out
    assert "msg-17" in out
    assert "msg-16" not in out


# ---------------------------------------------------------------------------
# PEEK_MESSAGE_MAX clamp — caller-supplied `n` is bounded by the formatter
# so the parent's peek response stays within budget regardless of misuse.
# ---------------------------------------------------------------------------


def test_peek_max_clamps_when_n_exceeds_cap():
    """Asking for more than PEEK_MESSAGE_MAX returns at most PEEK_MESSAGE_MAX
    distinct messages, even if the agent has many more."""
    msgs = [
        ConversationMessage(role="assistant", content=[TextBlock(text=f"msg-{i}")])
        for i in range(50)
    ]
    out = format_last_n_messages(msgs, n=999)
    # The newest message is always present; PEEK_MESSAGE_MAX older ones at most.
    assert f"msg-{50 - 1}" in out
    # Anything older than the (50 - PEEK_MESSAGE_MAX)-th message must be gone.
    cutoff = 50 - PEEK_MESSAGE_MAX
    for i in range(cutoff):
        assert f"msg-{i} " not in out and f"msg-{i}\n" not in out and f"msg-{i}…" not in out
    # And the boundary message (cutoff) IS present.
    assert f"msg-{cutoff}" in out


def test_peek_max_returns_exactly_cap_when_more_messages_exist():
    """When there are strictly more than PEEK_MESSAGE_MAX messages and the
    caller asks for n >= cap, exactly PEEK_MESSAGE_MAX messages are surfaced."""
    msgs = [
        ConversationMessage(role="assistant", content=[TextBlock(text=f"msg-{i}")])
        for i in range(PEEK_MESSAGE_MAX + 5)
    ]
    out = format_last_n_messages(msgs, n=PEEK_MESSAGE_MAX + 5)
    rendered = out.count("[text]")
    assert rendered == PEEK_MESSAGE_MAX, (
        f"expected exactly {PEEK_MESSAGE_MAX} rendered messages, got {rendered}: {out}"
    )


def test_peek_max_does_not_pad_when_messages_below_n():
    """If there are fewer messages than the cap (or the requested n), the
    formatter returns ALL messages — no padding, no fake entries."""
    msgs = [
        ConversationMessage(role="assistant", content=[TextBlock(text=f"only-{i}")])
        for i in range(3)
    ]
    out = format_last_n_messages(msgs, n=PEEK_MESSAGE_MAX)
    rendered = out.count("[text]")
    assert rendered == 3
    assert "only-0" in out
    assert "only-1" in out
    assert "only-2" in out


def test_peek_max_below_cap_honors_caller_n():
    """A caller asking for n < PEEK_MESSAGE_MAX still gets exactly n messages
    (the cap is an upper bound, not a target)."""
    msgs = [
        ConversationMessage(role="assistant", content=[TextBlock(text=f"m-{i}")])
        for i in range(20)
    ]
    out = format_last_n_messages(msgs, n=4)
    rendered = out.count("[text]")
    assert rendered == 4
    assert "m-19" in out
    assert "m-16" in out
    assert "m-15" not in out


def test_peek_max_constant_value_is_ten():
    """Sanity check on the cap itself — protects against accidental drift."""
    assert PEEK_MESSAGE_MAX == 10


def test_subagent_provider_clamps_via_format_helper():
    """End-to-end: when run_subagent's registered provider receives a runaway
    last_n via the bg manager, the rendered output still respects the cap."""
    msgs = [
        ConversationMessage(role="assistant", content=[TextBlock(text=f"e2e-{i}")])
        for i in range(30)
    ]
    out = format_last_n_messages(msgs, n=500)
    rendered = out.count("[text]")
    assert rendered == PEEK_MESSAGE_MAX
    # Newest survives, oldest beyond cap is gone.
    assert "e2e-29" in out
    assert "e2e-0" not in out


# ---------------------------------------------------------------------------
# Builtin subagent definition is registered
# ---------------------------------------------------------------------------


def test_builtin_subagent_is_registered():
    defn = get_agent_definition("scout")
    assert defn is not None
    assert defn.agent_type == "subagent"
    assert defn.name == "scout"
    assert defn.system_prompt
    assert "subagent" not in defn.toolkits  # cannot nest


def test_run_subagent_tool_flags():
    # New unified background-policy enum: "always" means the engine ALWAYS
    # dispatches this tool as a background task, regardless of LLM input.
    assert run_subagent.background == "always"


# ---------------------------------------------------------------------------
# run_subagent end-to-end with a stub spawn_agent
# ---------------------------------------------------------------------------


class _StubAgent:
    def __init__(
        self,
        scripted_messages: list[ConversationMessage],
        *,
        usage: UsageSnapshot | None = None,
        model: str = "mock-subagent-model",
    ) -> None:
        self._display_messages: list[ConversationMessage] = []
        self._scripted = scripted_messages
        self.total_usage = usage
        self.model = model
        self.agent_name = "scout"
        # Used by the test to inspect that progress provider sees live state.
        self.peek_calls: list[str] = []

    @property
    def display_messages(self) -> list[ConversationMessage]:
        return self._display_messages

    async def run(self, prompt: str):
        for msg in self._scripted:
            self._display_messages.append(msg)
            await asyncio.sleep(0)  # yield to allow inter-message peeks
            yield ("event",)

    async def close(self) -> None:
        pass


# ---------------------------------------------------------------------------
# Shared context/bg-manager helpers
# ---------------------------------------------------------------------------


class _StubCfg:
    cwd = Path("/tmp")
    session_id = "session_abc"
    external_api_client = object()


def _make_bg_manager(task_id: str, prompt: str = "task") -> BackgroundTaskManager:
    """Create a BackgroundTaskManager with one pre-launched noop task."""
    bg = BackgroundTaskManager()

    async def _noop_coro() -> ToolResult:
        return ToolResult(output="placeholder")

    bg.launch(
        task_id=task_id,
        tool_name="run_subagent",
        tool_input={"prompt": prompt},
        coro=_noop_coro(),
    )
    return bg


def _make_ctx(
    *,
    bg: BackgroundTaskManager | None = None,
    task_id: str | None = None,
    extra_meta: dict | None = None,
) -> ToolExecutionContext:
    """Build a ToolExecutionContext with standard defaults for run_subagent tests."""
    metadata: dict = {"session_config": _StubCfg()}
    if bg is not None:
        metadata["background_task_manager"] = bg
    if task_id is not None:
        metadata["background_task_id"] = task_id
    metadata["sandbox_id"] = ""
    metadata["agent_run_id"] = "parent_run_xyz"
    if extra_meta:
        metadata.update(extra_meta)
    return ToolExecutionContext(cwd=Path("/tmp"), metadata=metadata)


def _patch_stores(monkeypatch) -> tuple[object, object]:
    """Patch app_factory agent_run_store and usage_store; return (fake_store, fake_usage_store)."""
    fake_store = _StubAgentRunStore()
    fake_usage_store = _StubUsageStore()
    import server.app_factory as app_factory
    monkeypatch.setattr(app_factory, "agent_run_store", fake_store, raising=True)
    monkeypatch.setattr(app_factory, "usage_store", fake_usage_store, raising=True)
    return fake_store, fake_usage_store


@pytest.mark.asyncio
async def test_run_subagent_rejects_non_subagent_targets_with_plan_guidance(
    monkeypatch,
):
    class _NonSubagentDef:
        name = "developer"
        agent_type = "agent"

    monkeypatch.setattr("agents.get_definition", lambda _name: _NonSubagentDef())

    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"session_config": _StubCfg()},
    )

    result = await run_subagent.execute(
        run_subagent.input_model(agent_name="developer", prompt="run pytest"),
        ctx,
    )

    assert result.is_error is True
    assert "is not a subagent" in result.output
    assert "emit `developer` / `validator` tasks" in result.output


def test_validate_run_subagent_allows_multi_bucket_scout_bundle():
    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"session_config": _StubCfg()},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={"target_paths": ["dvc/command/diff.py", "dvc/repo/diff.py"]},
        context=ctx,
    )

    assert not isinstance(result, ToolResult)
    assert result.subagent_scope_paths == ["dvc/command/diff.py", "dvc/repo/diff.py"]


def test_validate_run_subagent_allows_same_bucket_scout_list():
    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"session_config": _StubCfg()},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={"target_paths": ["dvc/command/run.py", "dvc/command/repro.py"]},
        context=ctx,
    )

    assert not isinstance(result, ToolResult)
    assert result.subagent_scope_paths == ["dvc/command/run.py", "dvc/command/repro.py"]


@pytest.mark.parametrize(
    "parent_agent_name", ["root_planner", "team_planner", "team_replanner"]
)
def test_validate_run_subagent_rejects_planner_multi_path_scout_bundle(
    parent_agent_name: str,
):
    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"session_config": _StubCfg(), "agent_name": parent_agent_name},
    )
    target_paths = [
        "dask/dataframe/utils.py",
        "dask/dataframe/_compat.py",
        "dask/dataframe/__init__.py",
    ]

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={"target_paths": target_paths},
        context=ctx,
    )

    assert isinstance(result, ToolResult)
    assert result.is_error is True
    assert "must pass exactly one production owner path" in result.output
    assert "Split fan-out across multiple `run_subagent(...)` calls" in result.output


@pytest.mark.parametrize(
    "parent_agent_name", ["root_planner", "team_planner", "team_replanner"]
)
def test_validate_run_subagent_allows_single_path_planner_scout(
    parent_agent_name: str, tmp_path: Path
):
    _touch_rel(tmp_path, "dask/dataframe/io/json.py")
    ctx = ToolExecutionContext(
        cwd=tmp_path,
        metadata={"session_config": _StubCfg(), "agent_name": parent_agent_name},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={"target_paths": ["dask/dataframe/io/json.py"]},
        context=ctx,
    )

    assert not isinstance(result, ToolResult)
    assert result.subagent_scope_paths == ["dask/dataframe/io/json.py"]


@pytest.mark.parametrize(
    "parent_agent_name", ["root_planner", "team_planner", "team_replanner"]
)
def test_validate_run_subagent_rejects_planner_test_path_target(
    parent_agent_name: str, tmp_path: Path
):
    _touch_rel(tmp_path, "dask/tests/test_cli.py")
    ctx = ToolExecutionContext(
        cwd=tmp_path,
        metadata={"session_config": _StubCfg(), "agent_name": parent_agent_name},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={"target_paths": ["dask/tests/test_cli.py"]},
        context=ctx,
    )

    assert isinstance(result, ToolResult)
    assert result.is_error is True
    assert "must name one live production owner path" in result.output
    assert "Move benchmark tests" in result.output


@pytest.mark.parametrize(
    "parent_agent_name", ["root_planner", "team_planner", "team_replanner"]
)
def test_validate_run_subagent_rejects_planner_missing_target_path(
    parent_agent_name: str, tmp_path: Path
):
    ctx = ToolExecutionContext(
        cwd=tmp_path,
        metadata={"session_config": _StubCfg(), "agent_name": parent_agent_name},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={"target_paths": ["dask/dataframe/io/parquet.py"]},
        context=ctx,
    )

    assert isinstance(result, ToolResult)
    assert result.is_error is True
    assert "must name one live production owner path in the repo" in result.output
    assert "Missing path: dask/dataframe/io/parquet.py" in result.output


@pytest.mark.parametrize(
    "parent_agent_name", ["root_planner", "team_planner", "team_replanner"]
)
def test_validate_run_subagent_rejects_planner_context_with_other_owner_paths(
    parent_agent_name: str, tmp_path: Path
):
    for rel_path in (
        "dask/dataframe/io/parquet.py",
        "dask/dataframe/groupby.py",
        "dask/dataframe/utils.py",
        "dask/dataframe/io/json.py",
    ):
        _touch_rel(tmp_path, rel_path)
    ctx = ToolExecutionContext(
        cwd=tmp_path,
        metadata={"session_config": _StubCfg(), "agent_name": parent_agent_name},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={
            "target_paths": ["dask/dataframe/io/parquet.py"],
            "context": (
                "Confirm parquet, then also check dask/dataframe/groupby.py, "
                "dask/dataframe/utils.py, and dask/dataframe/io/json.py."
            ),
        },
        context=ctx,
    )

    assert isinstance(result, ToolResult)
    assert result.is_error is True
    assert "may not name other production owner paths" in result.output
    assert "dask/dataframe/groupby.py" in result.output


@pytest.mark.parametrize(
    "parent_agent_name", ["root_planner", "team_planner", "team_replanner"]
)
def test_validate_run_subagent_allows_planner_context_with_tests_only(
    parent_agent_name: str, tmp_path: Path
):
    _touch_rel(tmp_path, "dask/dataframe/io/hdf.py")
    ctx = ToolExecutionContext(
        cwd=tmp_path,
        metadata={"session_config": _StubCfg(), "agent_name": parent_agent_name},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={
            "target_paths": ["dask/dataframe/io"],
            "context": (
                "Failing tests include dask/dataframe/io/tests/test_hdf.py::test_to_hdf "
                "and dask/dataframe/io/tests/test_json.py::test_read_json_engine_str[ujson]. "
                "Confirm whether dask/dataframe/io/hdf.py exists under this directory."
            ),
        },
        context=ctx,
    )

    assert not isinstance(result, ToolResult)
    assert result.subagent_scope_paths == ["dask/dataframe/io"]


@pytest.mark.parametrize(
    "parent_agent_name", ["root_planner", "team_planner", "team_replanner"]
)
def test_validate_run_subagent_ignores_benchmark_variants_in_planner_context(
    parent_agent_name: str, tmp_path: Path
):
    _touch_rel(tmp_path, "dask/dataframe/groupby.py")
    ctx = ToolExecutionContext(
        cwd=tmp_path,
        metadata={"session_config": _StubCfg(), "agent_name": parent_agent_name},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={
            "target_paths": ["dask/dataframe/groupby.py"],
            "context": (
                "Failing variants include disk/tasks, disk/tasks-uint8, "
                "disk/tasks-uint8-by1/foo, 1/4/10-processes/sync/threads, "
                "fastparquet/pyarrow, pyarrow-pandas/pyarrow, and "
                "config_get/config_list."
            ),
        },
        context=ctx,
    )

    assert not isinstance(result, ToolResult)
    assert result.subagent_scope_paths == ["dask/dataframe/groupby.py"]


def test_validate_run_subagent_allows_all_test_file_scout():
    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"session_config": _StubCfg()},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={"target_paths": [
            "tests/unit/command/test_diff.py",
            "tests/unit/command/test_plots.py",
        ]},
        context=ctx,
    )

    assert not isinstance(result, ToolResult)
    assert result.subagent_scope_paths == [
        "tests/unit/command/test_diff.py",
        "tests/unit/command/test_plots.py",
    ]


def test_validate_run_subagent_allows_mixed_prod_and_test_scout():
    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"session_config": _StubCfg()},
    )

    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={"target_paths": [
            "dvc/command/diff.py",
            "tests/unit/command/test_diff.py",
        ]},
        context=ctx,
    )

    assert not isinstance(result, ToolResult)
    assert result.subagent_scope_paths == [
        "dvc/command/diff.py",
        "tests/unit/command/test_diff.py",
    ]


def test_run_subagent_schema_is_agent_agnostic():
    """The tool schema must not hardcode scout-specific payload prose; each
    dispatchable subagent owns its own contract documentation."""
    schema = run_subagent.to_api_schema()

    # Outer description stays generic — no scout-only terminology.
    description = schema["description"]
    assert "scout" not in description.lower()
    assert "target_paths" not in description
    assert "benchmark" not in description.lower()
    # It still names the high-level dispatch contract.
    assert "dispatchable subagent" in description
    assert "prompt" in description and "input" in description

    input_description = schema["input_schema"]["properties"]["input"]["description"]
    assert "scout" not in input_description.lower()
    assert "target_paths" not in input_description
    assert "subagent's own contract" in input_description

    # Non-planner callers can still mix a production path and a test path
    # for a scout call — the runtime gate only fires for planner-tier callers.
    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={"session_config": _StubCfg()},
    )
    result = _validate_run_subagent_request(
        agent_name="scout",
        prompt=None,
        input={
            "target_paths": [
                "dvc/command/diff.py",
                "tests/unit/command/test_diff.py",
            ]
        },
        context=ctx,
    )

    assert not isinstance(result, ToolResult)


@pytest.mark.asyncio
async def test_run_subagent_registers_provider_and_returns_final_text(monkeypatch):
    scripted = [
        ConversationMessage(role="user", content=[TextBlock(text="task")]),
        ConversationMessage(
            role="assistant",
            content=[
                ToolUseBlock(name="read_file", input={"path": "x"}),
            ],
        ),
        ConversationMessage(
            role="assistant",
            content=[TextBlock(text="DONE: refactored module X")],
        ),
    ]

    stub_agent = _StubAgent(
        scripted,
        usage=UsageSnapshot(input_tokens=21, output_tokens=9),
    )
    monkeypatch.setattr(
        "engine.runtime.agent.spawn_agent", lambda *a, **kw: stub_agent, raising=True
    )

    bg = _make_bg_manager("bg_test")
    ctx = _make_ctx(bg=bg, task_id="bg_test")

    result = await run_subagent.execute(
        run_subagent.input_model(agent_name="scout", prompt="task"), ctx
    )

    assert result.is_error is False
    assert "DONE" in result.output
    assert "refactored module X" in result.output

    # Provider should have been registered.
    tracked = bg._tasks["bg_test"]
    assert tracked.progress_provider is not None
    snapshot = tracked.progress_provider(5)
    assert isinstance(snapshot, str)
    assert "[text]" in snapshot or "[tool]" in snapshot




@pytest.mark.asyncio
async def test_run_subagent_does_not_inject_scout_preamble(monkeypatch):
    """The dispatcher must pass the caller's payload through verbatim;
    scope/scout rules belong to the scout's own playbook, not to
    ``run_subagent``."""
    import json

    scripted = [
        ConversationMessage(role="assistant", content=[TextBlock(text="DONE: scoped read complete")]),
    ]
    stub_agent = _StubAgent(scripted)
    captured: dict[str, str] = {}

    def _fake_spawn_agent(*args, **kwargs):
        captured["prompt"] = kwargs["latest_user_prompt"]
        return stub_agent

    monkeypatch.setattr(
        "engine.runtime.agent.spawn_agent", _fake_spawn_agent, raising=True
    )

    bg = _make_bg_manager("bg_no_preamble")
    ctx = _make_ctx(bg=bg, task_id="bg_no_preamble")

    payload = {"target_paths": ["pkg/config.py"]}
    result = await run_subagent.execute(
        run_subagent.input_model(agent_name="scout", input=payload),
        ctx,
    )

    assert result.is_error is False
    # The final prompt handed to spawn_agent is the serialized input with
    # no dispatcher-side scope contract prepended.
    expected = json.dumps(payload, separators=(",", ":"), default=str)
    assert captured["prompt"] == expected
    # And specifically none of the old scout-preamble phrases leak through.
    forbidden = [
        "Scout scope contract",
        "stay read-free and post `submit_file_notes(...)` from CI evidence",
        "do not fan out into generic symbol hunts",
        "exact-file and short fixed-file scouts stay read-free",
    ]
    for phrase in forbidden:
        assert phrase not in captured["prompt"], f"leaked scout preamble: {phrase!r}"


@pytest.mark.asyncio
async def test_run_subagent_missing_session_config_returns_error():
    ctx = ToolExecutionContext(cwd=Path("/tmp"), metadata={})
    result = await run_subagent.execute(
        run_subagent.input_model(agent_name="scout", prompt="task"), ctx
    )
    assert result.is_error is True
    assert "session_config" in result.output


@pytest.mark.asyncio
async def test_run_subagent_provider_error_is_caught():
    # Verify the bg manager swallows progress provider exceptions and surfaces
    # them as a [progress provider error] string instead of crashing.
    bg = BackgroundTaskManager()

    async def _noop_coro() -> ToolResult:
        return ToolResult(output="placeholder")

    bg.launch(
        task_id="bg_err",
        tool_name="x",
        tool_input={},
        coro=_noop_coro(),
    )

    def _bad_provider(last_n: int) -> str:
        raise RuntimeError("boom")

    bg.set_progress_provider("bg_err", _bad_provider)
    statuses = bg.get_status("bg_err")
    assert len(statuses) == 1
    assert "[progress provider error" in statuses[0]["output"]
    assert "boom" in statuses[0]["output"]


# ---------------------------------------------------------------------------
# Persistence: subagent runs are recorded with parent_run_id / parent_task_id
# ---------------------------------------------------------------------------


class _StubAgentRunStore:
    """Captures create_run / finish_run kwargs for test inspection."""

    def __init__(self) -> None:
        self._session_factory = object()  # truthy → "DB available"
        self.created: list[dict] = []
        self.finished: list[dict] = []

    @property
    def is_ready(self) -> bool:
        return self._session_factory is not None

    def create_run(self, **kwargs):
        self.created.append(kwargs)
        return None

    def finish_run(self, run_id, **kwargs):
        self.finished.append({"run_id": run_id, **kwargs})
        return None


class _StubUsageStore:
    def __init__(self) -> None:
        self.records: list[dict] = []

    def record(self, **kwargs):
        self.records.append(kwargs)
        return kwargs


@pytest.mark.asyncio
async def test_run_subagent_persists_run_with_parent_ids(monkeypatch):
    """run_subagent must call create_run with parent_run_id + parent_task_id
    derived from context.metadata, and finish_run with status='completed'
    and the inner agent's compacted_history."""
    scripted = [
        ConversationMessage(role="user", content=[TextBlock(text="task")]),
        ConversationMessage(
            role="assistant", content=[TextBlock(text="DONE: child output")]
        ),
    ]
    stub_agent = _StubAgent(
        scripted,
        usage=UsageSnapshot(input_tokens=21, output_tokens=9),
    )
    monkeypatch.setattr(
        "engine.runtime.agent.spawn_agent",
        lambda *a, **kw: stub_agent,
        raising=True,
    )

    fake_store, fake_usage_store = _patch_stores(monkeypatch)
    bg = _make_bg_manager("bg_persist")
    ctx = _make_ctx(bg=bg, task_id="bg_persist")

    result = await run_subagent.execute(
        run_subagent.input_model(agent_name="scout", prompt="do the thing"), ctx
    )

    assert result.is_error is False
    assert "DONE: child output" in result.output

    # create_run was called with the parent ids and parent's session_id.
    assert len(fake_store.created) == 1
    create_kwargs = fake_store.created[0]
    assert create_kwargs["session_id"] == "session_abc"
    assert create_kwargs["parent_run_id"] == "parent_run_xyz"
    assert create_kwargs["parent_task_id"] == "bg_persist"
    assert create_kwargs["agent_name"] == "scout"
    assert create_kwargs["input_query"] == "do the thing"

    # finish_run was called with completed status. The full display history
    # is persisted to message_history; compacted_history holds the last
    # api_messages snapshot (None when the stub agent doesn't expose a
    # query_context).
    assert len(fake_store.finished) == 1
    finish_kwargs = fake_store.finished[0]
    assert finish_kwargs["status"] == "completed"
    assert finish_kwargs["error"] is None
    assert finish_kwargs["message_history"]
    assert finish_kwargs["response"] == {"final_text": "DONE: child output"}
    assert len(fake_usage_store.records) == 1
    usage_kwargs = fake_usage_store.records[0]
    assert usage_kwargs["session_id"] == "session_abc"
    assert usage_kwargs["run_id"] == finish_kwargs["run_id"]
    assert usage_kwargs["agent_name"] == "scout"
    assert usage_kwargs["model_id"] == getattr(stub_agent, "model", "")
    assert usage_kwargs["prompt_tokens"] == 21
    assert usage_kwargs["completion_tokens"] == 9


@pytest.mark.asyncio
async def test_run_subagent_persists_spawn_failure(monkeypatch):
    """If spawn_agent itself raises (before the worker ever starts), the run
    record must still be created AND finalized as `failed` so the parent has
    an audit row to inspect / retry."""

    def _boom(*args, **kwargs):
        raise RuntimeError("spawn boom")

    monkeypatch.setattr("engine.runtime.agent.spawn_agent", _boom, raising=True)

    fake_store, fake_usage_store = _patch_stores(monkeypatch)

    ctx = ToolExecutionContext(
        cwd=Path("/tmp"),
        metadata={
            "session_config": _StubCfg(),
            "background_task_id": "bg_spawn_fail",
            "agent_run_id": "parent_run_xyz",
        },
    )

    result = await run_subagent.execute(run_subagent.input_model(agent_name="scout", prompt="x"), ctx)

    assert result.is_error is True
    assert "spawn failed" in result.output
    # The run row was created BEFORE spawn was attempted.
    assert len(fake_store.created) == 1
    assert fake_store.created[0]["parent_run_id"] == "parent_run_xyz"
    assert fake_store.created[0]["parent_task_id"] == "bg_spawn_fail"
    # And finalized as failed at the spawn stage.
    assert len(fake_store.finished) == 1
    assert fake_store.finished[0]["status"] == "failed"
    assert "spawn boom" in fake_store.finished[0]["error"]
    assert fake_usage_store.records == []


@pytest.mark.asyncio
async def test_run_subagent_persists_failure(monkeypatch):
    """If the inner agent crashes, finish_run should be called with
    status='failed' and error=<exc message>, and the tool result is_error=True."""

    class _CrashingAgent(_StubAgent):
        def __init__(self) -> None:
            super().__init__(
                [],
                usage=UsageSnapshot(input_tokens=13, output_tokens=7),
            )

        async def run(self, prompt: str):
            self._display_messages.append(
                ConversationMessage(role="user", content=[TextBlock(text=prompt)])
            )
            yield ("step",)
            raise RuntimeError("inner exploded")

    monkeypatch.setattr(
        "engine.runtime.agent.spawn_agent",
        lambda *a, **kw: _CrashingAgent(),
        raising=True,
    )

    fake_store, fake_usage_store = _patch_stores(monkeypatch)
    bg = _make_bg_manager("bg_fail", prompt="x")
    ctx = _make_ctx(bg=bg, task_id="bg_fail")

    result = await run_subagent.execute(run_subagent.input_model(agent_name="scout", prompt="x"), ctx)

    assert result.is_error is True
    assert "inner exploded" in result.output
    assert len(fake_store.finished) == 1
    assert fake_store.finished[0]["status"] == "failed"
    assert "inner exploded" in fake_store.finished[0]["error"]
    assert len(fake_usage_store.records) == 1
    assert fake_usage_store.records[0]["run_id"] == fake_store.finished[0]["run_id"]
    assert fake_usage_store.records[0]["prompt_tokens"] == 13
    assert fake_usage_store.records[0]["completion_tokens"] == 7


@pytest.mark.asyncio
async def test_run_subagent_persists_usage_when_cancelled(monkeypatch):
    class _CancelledAgent(_StubAgent):
        def __init__(self) -> None:
            super().__init__(
                [ConversationMessage(role="user", content=[TextBlock(text="x")])],
                usage=UsageSnapshot(input_tokens=8, output_tokens=2),
            )

        async def run(self, prompt: str):
            self._display_messages.append(
                ConversationMessage(role="user", content=[TextBlock(text=prompt)])
            )
            yield ("event",)
            raise asyncio.CancelledError("cancelled")

    monkeypatch.setattr(
        "engine.runtime.agent.spawn_agent",
        lambda *a, **kw: _CancelledAgent(),
        raising=True,
    )

    fake_store, fake_usage_store = _patch_stores(monkeypatch)
    bg = _make_bg_manager("bg_cancel", prompt="x")
    tracked = bg.get_task("bg_cancel")
    assert tracked is not None
    tracked.status = "cancelled"
    tracked.cancel_reason = "user requested stop"

    ctx = _make_ctx(bg=bg, task_id="bg_cancel")

    with pytest.raises(asyncio.CancelledError):
        await run_subagent.execute(run_subagent.input_model(agent_name="scout", prompt="x"), ctx)

    assert len(fake_store.finished) == 1
    assert fake_store.finished[0]["status"] == "cancelled"
    assert fake_store.finished[0]["cancellation_reason"] == "user requested stop"
    assert len(fake_usage_store.records) == 1
    assert fake_usage_store.records[0]["prompt_tokens"] == 8
    assert fake_usage_store.records[0]["completion_tokens"] == 2
