"""A18 — Live e2e smoke tests for coding-plan mode.

Sibling of ``test_real_agent.py``. Covers the v9 A18 acceptance criterion:
prove that coding-plan-mode (Anthropic OAuth + Codex OAuth) composes end-to-end
with real sandboxes AND with our fully-customizable EphemeralOS tools.

**Activation gates (all must pass for a given test to run):**

1. ``EOS_SWEEVO_REAL_AGENT_TESTS=1`` (module-level — matches existing pattern).
2. Provider-specific credentials present (per-test):
   * Anthropic test: macOS Keychain entry ``Claude Code-credentials``.
   * Codex test: ``~/.codex/auth.json``.
3. Coding-plan-mode infrastructure exists (per-test): the
   ``providers.clients.coding_plan`` package importable AND a coding-plan-mode-
   capable ``class_path`` value supported by ``make_api_client``. Until
   Phase 1 lands this gate skips automatically — the test file lives
   permanently on main without breaking CI.

**Why the test file lands BEFORE Phase 1:**

- Auto-skip gates mean zero CI cost today.
- Once Phase 1+3 land, the gates auto-flip and the tests start running
  under ``EOS_SWEEVO_REAL_AGENT_TESTS=1``; no separate scaffolding PR.
- The skip-reasons themselves document the missing pieces for any future
  contributor reading the file.

Plan reference: ``.planning/coding_plan_mode_plan.md`` A18.
"""

from __future__ import annotations

import importlib
import os
import subprocess
from pathlib import Path

import pytest

# Module-level gate: mirrors test_real_agent.py.
pytestmark = pytest.mark.skipif(
    os.getenv("EOS_SWEEVO_REAL_AGENT_TESTS") != "1",
    reason="Real-agent live e2e gated by EOS_SWEEVO_REAL_AGENT_TESTS=1",
)


# ---------------------------------------------------------------------------
# Skip-gate helpers (queried per test).
# ---------------------------------------------------------------------------


def _anthropic_keychain_present() -> bool:
    """Macos-only check for Claude Code OAuth credentials in Keychain."""
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
    """True iff Phase 1+ landed the coding_plan client package.

    Detection strategy: attempt to import the package. The package is
    introduced by the Phase 1 refactor (per v8 §A5 / v9 §6.5 / namespace
    layout in v6). Until it exists, all three tests skip.
    """
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
    reason="Coding-plan-mode infrastructure not yet landed (Phase 1 dependency)",
)


# ---------------------------------------------------------------------------
# Tests.
# ---------------------------------------------------------------------------


@_SKIP_NO_ANTHROPIC_CREDS
@_SKIP_NO_PLAN_INFRA
@pytest.mark.asyncio
async def test_anthropic_coding_plan_mode_e2e() -> None:
    """A18.1 — Anthropic coding-plan-mode + custom tools end-to-end.

    Activation: needs Phase 1 (AnthropicClient refactor, class_path
    dispatch, A11 audit field, A17 observability) AND Phase 3 (sweevo
    capability-parity benchmark wiring).

    Once Phase 1+3 land, this test will:
      1. Register a coding-plan-mode ``model_registrations`` row with
         ``class_path="providers.clients.api.anthropic_native:AnthropicClient"``
         and ``kwargs_json={"auth": "claude_oauth"}``.
      2. Run a canonical SWE-EVO instance via ``run_sweevo_real_agent``.
      3. Assert ``coding_plan_mode_active=true`` in ``run.json`` (A11).
      4. Assert at least one tool_use record of a custom EphemeralOS
         tool (e.g. ``read_file`` or ``shell``).
      5. Assert the SWE-EVO sandbox-diff envelope matches expectations.
      6. Assert NO ``coding_plan_mode_error`` log lines emitted at 4xx/5xx (A17).
    """
    pytest.skip(
        "A18.1 implementation pending Phase 1 (coding-plan-mode registration "
        "helper) + Phase 3 (sweevo coding-plan-mode parity hooks). Scaffold "
        "intentionally lands ahead so the file is reachable from CI once "
        "the gates flip."
    )


@_SKIP_NO_CODEX_CREDS
@_SKIP_NO_PLAN_INFRA
@pytest.mark.asyncio
async def test_codex_coding_plan_mode_e2e() -> None:
    """A18.2 — Codex coding-plan-mode + custom tools end-to-end.

    Equivalent of A18.1 with
    ``class_path="providers.clients.coding_plan.codex:CodexResponsesClient"``
    and same assertion set.
    """
    pytest.skip(
        "A18.2 implementation pending Phase 2 (CodexResponsesClient) + "
        "Phase 3. Gated separately from A18.1 so an Anthropic-only or "
        "Codex-only smoke is possible."
    )


@pytest.mark.asyncio
async def test_api_mode_regression() -> None:
    """A18.3 — Existing API-mode flow runs unchanged when no coding-plan-mode row is active.

    Skipped today because it duplicates ``test_real_agent.py``; included
    as an explicit slot so a future Phase 1 regression can land here
    rather than diluting the existing test. Once Phase 1 lands, this
    test will assert ``coding_plan_mode_active=false`` in ``run.json`` and zero
    ``coding_plan_mode_error`` log lines.
    """
    pytest.skip(
        "A18.3 — Duplicates test_real_agent.py until Phase 1 lands the "
        "coding_plan_mode_active audit field. Will be activated alongside A11."
    )
