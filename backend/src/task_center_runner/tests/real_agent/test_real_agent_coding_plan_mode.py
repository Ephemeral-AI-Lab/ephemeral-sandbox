"""A18 — Live e2e smoke tests for coding-plan mode.

Sibling of ``test_real_agent.py``. Covers the v9 A18 acceptance criterion:
prove that coding-plan-mode (Anthropic OAuth + Codex OAuth) composes end-to-end
with real sandboxes AND with our fully-customizable EphemeralOS tools.

Run explicitly through the ``tests/real_agent`` suite. Per-test skip gates still
protect provider-specific credentials and optional coding-plan-mode
infrastructure.

**Activation gates (per-test):**

1. Provider-specific credentials present:
   * Anthropic test: macOS Keychain entry ``Claude Code-credentials``.
   * Codex test: ``~/.codex/auth.json``.
2. Coding-plan-mode infrastructure exists: the
   ``providers.clients.coding_plan`` package importable.

Plan reference: ``.planning/final_phase_live_e2e_plan.md`` S7 (mirrors
``.planning/coding_plan_mode_plan.md`` A18).
"""

from __future__ import annotations

import importlib
import json
import logging
import subprocess
import uuid
from collections.abc import Iterator
from pathlib import Path
from typing import Any, Callable

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from task_center_runner.core.real_agent_run import run_sweevo_real_agent
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.tests._live_config import real_agent_max_duration_s
from tools.sandbox._lib.registry import make_sandbox_tools

pytestmark = pytest.mark.real_agent


# ---------------------------------------------------------------------------
# Skip-gate helpers (queried per test).
# ---------------------------------------------------------------------------


def _anthropic_keychain_present() -> bool:
    """Macos-only check for Claude Code OAuth credentials in Keychain."""
    import os

    if os.uname().sysname != "Darwin":  # type: ignore[attr-defined]
        return False
    user = os.environ.get("USER")
    if not user:
        return False
    try:
        result = subprocess.run(
            [
                "security",
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-a",
                user,
            ],
            capture_output=True,
            text=True,
            check=False,
        )
    except FileNotFoundError:
        return False
    return result.returncode == 0


def _codex_credentials_present() -> bool:
    return (Path.home() / ".codex" / "auth.json").exists()


def _plan_mode_infrastructure_present() -> bool:
    """True iff the coding_plan client package is importable."""
    try:
        importlib.import_module("providers.clients.coding_plan")
    except ImportError:
        return False
    return True


_SKIP_NO_ANTHROPIC_CREDS = pytest.mark.skipif(
    not _anthropic_keychain_present(),
    reason="Anthropic OAuth keychain entry not present",
)
_SKIP_NO_CODEX_CREDS = pytest.mark.skipif(
    not _codex_credentials_present(),
    reason="Codex ~/.codex/auth.json not present",
)
_SKIP_NO_PLAN_INFRA = pytest.mark.skipif(
    not _plan_mode_infrastructure_present(),
    reason="Coding-plan-mode infrastructure not yet landed",
)


# ---------------------------------------------------------------------------
# Fixtures.
# ---------------------------------------------------------------------------


@pytest.fixture
def _register_plan_mode_row(
    stores: TaskCenterStoreBundle,
) -> Iterator[Callable[..., str]]:
    """Register a coding-plan-mode ``model_registrations`` row for one test.

    See ``db/stores/model_store.py:98-176`` for the register/delete API.
    The fixture rebinds the global ``model_store`` singleton onto the
    per-test session factory and restores the prior binding on teardown
    so subsequent tests don't query a dropped per-test schema.
    """
    from runtime.app_factory import model_store

    prior_sf = model_store._session_factory  # may be None on first use
    model_store.initialize(stores.session_factory)
    registered_keys: list[str] = []

    def _register(class_path: str, kwargs_extra: dict | None = None) -> str:
        key = f"test/plan-mode-{uuid.uuid4().hex[:8]}"
        model_store.register(
            key=key,
            label="Test Plan-Mode Row",
            class_path=class_path,
            kwargs=kwargs_extra or {},
            activate=True,
        )
        registered_keys.append(key)
        return key

    yield _register

    for key in registered_keys:
        try:
            model_store.delete(key)
        except Exception:
            pass
    # Restore pre-fixture binding so the next test's bootstrap path
    # initializes model_store cleanly (or sees the existing shared
    # binding) instead of querying this dropped per-test schema.
    model_store._session_factory = prior_sf


# ---------------------------------------------------------------------------
# Shared assertion helpers.
# ---------------------------------------------------------------------------


def _setup_caplog(caplog: pytest.LogCaptureFixture) -> None:
    """Capture ERROR-level records on BOTH vendor loggers.

    Single ``set_level`` on one logger won't catch the other vendor's
    records — both providers emit ``coding_plan_mode_error`` against their
    own module logger.
    """
    caplog.set_level(logging.ERROR, logger="providers.clients.anthropic_native")
    caplog.set_level(logging.ERROR, logger="providers.clients.coding_plan.codex")


def _assert_outcome_shape(report: Any) -> None:
    """Outcome-shape assertions verbatim from ``test_real_agent.py:42-49``."""
    assert report.task_center_run_id
    assert report.run_dir.is_dir()
    assert (report.run_dir / "run.json").is_file()
    assert (report.run_dir / "sweevo_result.json").is_file()
    assert report.task_center_status in {"done", "failed", "cancelled"}
    if report.task_center_status == "done" and not report.aborted_by_timeout:
        assert report.sweevo_result.fail_to_pass_total > 0


def _assert_coding_plan_mode_active(run_dir: Path, expected: bool) -> None:
    run_json = json.loads((run_dir / "run.json").read_text())
    actual = run_json["coding_plan_mode_active"]
    if expected:
        assert actual is True, (
            f"coding_plan_mode_active expected True, got {actual!r}"
        )
    else:
        assert actual is False, (
            f"coding_plan_mode_active expected False, got {actual!r}"
        )


def _collect_tool_use_names(run_dir: Path) -> set[str]:
    """Walk ``message.jsonl`` files under ``run_dir`` and collect tool_use names.

    Schema: ``backend/src/message/agent_message_recorder.py`` writes JSONL
    rows of ``{"role": ..., "content": [block_dict, ...], "metadata": ...}``;
    tool_use blocks have ``{"type": "tool_use", "id": ..., "name": ..., "input": ...}``.
    """
    names: set[str] = set()
    for jsonl_path in run_dir.rglob("message.jsonl"):
        try:
            lines = jsonl_path.read_text(encoding="utf-8").splitlines()
        except OSError:
            continue
        for line in lines:
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            for block in event.get("content", []) or []:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    name = block.get("name")
                    if isinstance(name, str) and name:
                        names.add(name)
    return names


def _assert_sandbox_tool_routing(run_dir: Path) -> None:
    """Assert at least one tool_use name is in the sandbox-tool set.

    Routing-regression signal — catches a future bug where the agent
    routes through a non-sandbox tool registry. Capability is proven
    separately by ``fail_to_pass_total > 0``.
    """
    sandbox_tool_names = {tool.name for tool in make_sandbox_tools()}
    observed = _collect_tool_use_names(run_dir)
    overlap = observed & sandbox_tool_names
    assert overlap, (
        f"Expected at least one sandbox-tool tool_use under {run_dir}; "
        f"observed={sorted(observed)}, sandbox-set={sorted(sandbox_tool_names)}"
    )


def _assert_no_coding_plan_mode_error(caplog: pytest.LogCaptureFixture) -> None:
    """Assert zero ``coding_plan_mode_error`` records.

    Producer calls ``log.error("coding_plan_mode_error", extra=...)`` with
    no format args, so ``record.message`` equals the raw event name.
    """
    records = [r for r in caplog.records if r.message == "coding_plan_mode_error"]
    assert len(records) == 0, (
        f"Expected zero coding_plan_mode_error records, got {len(records)}: "
        f"{[(r.name, getattr(r, 'provider', None), getattr(r, 'error_type', None)) for r in records]}"
    )


def _max_duration_s() -> float:
    return real_agent_max_duration_s()


# ---------------------------------------------------------------------------
# Tests.
# ---------------------------------------------------------------------------


@_SKIP_NO_ANTHROPIC_CREDS
@_SKIP_NO_PLAN_INFRA
@pytest.mark.asyncio
async def test_anthropic_coding_plan_mode_e2e(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
    caplog: pytest.LogCaptureFixture,
    _register_plan_mode_row: Callable[..., str],
) -> None:
    """A18.1 — Anthropic coding-plan-mode + custom tools end-to-end.

    Registers an Anthropic plan-mode row, runs the canonical SWE-EVO instance
    via ``run_sweevo_real_agent``, asserts outcome shape + A11 audit field +
    sandbox-tool routing-regression + zero A17 error logs.
    """
    _setup_caplog(caplog)
    _register_plan_mode_row(
        class_path="providers.clients.coding_plan.anthropic:AnthropicPlanClient",
        kwargs_extra={"model": "claude-sonnet-4-5"},
    )

    report = await run_sweevo_real_agent(
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
        max_duration_s=_max_duration_s(),
    )

    _assert_outcome_shape(report)
    _assert_coding_plan_mode_active(report.run_dir, expected=True)
    _assert_sandbox_tool_routing(report.run_dir)
    _assert_no_coding_plan_mode_error(caplog)


@_SKIP_NO_CODEX_CREDS
@_SKIP_NO_PLAN_INFRA
@pytest.mark.asyncio
async def test_codex_coding_plan_mode_e2e(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
    caplog: pytest.LogCaptureFixture,
    _register_plan_mode_row: Callable[..., str],
) -> None:
    """A18.2 — Codex coding-plan-mode + custom tools end-to-end.

    Same shape as the Anthropic test with the Codex class_path. Model
    auto-resolves from ``~/.codex/config.toml`` (defaults to ``gpt-5.5``).
    """
    _setup_caplog(caplog)
    # ``model`` must be set for ``factory._resolve_agent_identity`` to
    # accept the row (factory rejects empty model id at line 185 before
    # the client is constructed). ``CodexResponsesClient`` will still
    # auto-resolve its actual model from ``~/.codex/config.toml`` — this
    # value just satisfies the factory's pre-construction validation.
    _register_plan_mode_row(
        class_path="providers.clients.coding_plan.codex:CodexResponsesClient",
        kwargs_extra={"model": "gpt-5.5"},
    )

    report = await run_sweevo_real_agent(
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
        max_duration_s=_max_duration_s(),
    )

    _assert_outcome_shape(report)
    _assert_coding_plan_mode_active(report.run_dir, expected=True)
    _assert_sandbox_tool_routing(report.run_dir)
    _assert_no_coding_plan_mode_error(caplog)


@pytest.mark.asyncio
async def test_api_mode_regression(
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
    caplog: pytest.LogCaptureFixture,
) -> None:
    """A18.3 — API-mode regression: no plan-mode row registered.

    Asserts ``coding_plan_mode_active`` is False and zero
    ``coding_plan_mode_error`` log records (api-mode does not emit them).
    Pairs with the two plan-mode tests above so the three-mode parity
    benchmark (S8) can run from one test file.
    """
    _setup_caplog(caplog)

    report = await run_sweevo_real_agent(
        instance=sweevo_image_instance,
        sandbox_id=str(workspace["sandbox_id"]),
        audit_dir=audit_dir,
        stores=stores,
        max_duration_s=_max_duration_s(),
    )

    _assert_outcome_shape(report)
    _assert_coding_plan_mode_active(report.run_dir, expected=False)
    _assert_no_coding_plan_mode_error(caplog)
