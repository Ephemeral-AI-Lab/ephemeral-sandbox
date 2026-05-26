"""Phase 4 §AC10/§G1: tests for the dispatch-callsite lint guard."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path



_REPO_ROOT = Path(__file__).resolve().parents[5]
_LINT_SCRIPT = _REPO_ROOT / "backend" / "tools" / "lint_dispatch_callsites.py"


def _load_lint_module():
    """Load the lint script by absolute path because ``backend/tools/`` is
    not on the project's import path (the wheel only ships ``backend/src/``)."""
    cached = sys.modules.get("_phase4_lint_dispatch_callsites")
    if cached is not None:
        return cached
    spec = importlib.util.spec_from_file_location(
        "_phase4_lint_dispatch_callsites", _LINT_SCRIPT
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["_phase4_lint_dispatch_callsites"] = module
    spec.loader.exec_module(module)
    return module


_lint_mod = _load_lint_module()
lint = _lint_mod.lint
_RULES = _lint_mod._RULES


def test_lint_dispatch_callsites_baseline_passes():
    """The real repo must pass the lint as it stands today."""
    violations = lint(_REPO_ROOT)
    assert violations == [], "\n".join(violations)


def test_lint_dispatch_callsites_extra_caller_fails(tmp_path: Path):
    """Adding a synthetic caller outside the allowed roots must fail."""
    # Build a minimal repo layout that satisfies the lint's structure
    # checks. ``backend/src/sandbox/daemon/`` (the allowed root for
    # ``dispatch_workspace_tool_call``) is empty; ``backend/src/other/``
    # holds the synthetic extra caller. ``backend/tests/`` exists so the
    # tests-root scan does not short-circuit.
    src = tmp_path / "backend" / "src"
    (src / "sandbox" / "daemon").mkdir(parents=True)
    (src / "sandbox" / "daemon" / "workspace_tool_dispatch.py").write_text(
        "def dispatch_workspace_tool_call(args, *, verb, intent):\n    return None\n",
        encoding="utf-8",
    )
    (src / "sandbox" / "daemon" / "rpc").mkdir(parents=True)
    (src / "sandbox" / "daemon" / "rpc" / "dispatcher.py").write_text(
        "def _plugin_block_decision(op, agent_id):\n    return None\n",
        encoding="utf-8",
    )
    other = src / "other"
    other.mkdir(parents=True)
    (other / "rogue.py").write_text(
        "from sandbox.daemon.workspace_tool_dispatch import dispatch_workspace_tool_call\n"
        "def caller():\n"
        "    return dispatch_workspace_tool_call({}, verb='x', intent=None)\n",
        encoding="utf-8",
    )
    (tmp_path / "backend" / "tests").mkdir(parents=True)

    violations = lint(tmp_path)
    assert violations, "expected a violation, got none"
    assert any("dispatch_workspace_tool_call" in v for v in violations)


def test_lint_dispatch_callsites_extra_plugin_gate_caller_fails(tmp_path: Path):
    """Adding a synthetic caller of ``_plugin_block_decision`` must fail."""
    src = tmp_path / "backend" / "src"
    (src / "sandbox" / "daemon").mkdir(parents=True)
    (src / "sandbox" / "daemon" / "workspace_tool_dispatch.py").write_text(
        "def dispatch_workspace_tool_call(args, *, verb, intent):\n    return None\n",
        encoding="utf-8",
    )
    (src / "sandbox" / "daemon" / "rpc").mkdir(parents=True)
    (src / "sandbox" / "daemon" / "rpc" / "dispatcher.py").write_text(
        "def _plugin_block_decision(op, agent_id):\n    return None\n",
        encoding="utf-8",
    )
    other = src / "other"
    other.mkdir(parents=True)
    (other / "rogue_plugin.py").write_text(
        "from sandbox.daemon.rpc.dispatcher import _plugin_block_decision\n"
        "def caller():\n"
        "    return _plugin_block_decision('plugin.x', 'agent')\n",
        encoding="utf-8",
    )
    (tmp_path / "backend" / "tests").mkdir(parents=True)

    violations = lint(tmp_path)
    assert violations
    assert any("_plugin_block_decision" in v for v in violations)


def test_lint_dispatch_callsites_rules_cover_phase4_symbols():
    """Sanity: both symbols protected by Phase 4 §G1 are in the rule set."""
    names = {symbol for symbol, _, _ in _RULES}
    assert "dispatch_workspace_tool_call" in names
    assert "_plugin_block_decision" in names


__all__ = ()
