"""A8 — Audit recorder must not import coding-plan-mode storage or client packages.

Per `.planning/coding_plan_mode_plan.md` v8 §A8, the audit recorder is the
load-bearing privacy boundary for coding-plan-mode OAuth tokens. If
``recorder.py`` were to import any of:

* ``providers.clients.coding_plan.*`` (any coding-plan-mode client module), or
* ``db.stores.model_store`` (the persistence layer that holds
  ``model_registrations.kwargs_json``, which contains the strategy
  selector for OAuth modes),

then an accidental ``__repr__`` / ``str()`` / ``asdict()`` call on a
coding-plan-mode object inside the recorder could leak a token literal
into a JSONL audit file. The static-graph guard enforces that no such
reach exists in the first place.

This test follows the same pattern as
``test_no_core_imports.py`` (Phase 4c invariant guard).

The property is TRUE today (recorder.py imports only ``db.models.*`` and
``task_center_runner.audit.*``) — this guard lands now to prevent
regression once Phase 1 lands and `provider.py`/`engine.py` start carrying
coding-plan-mode-aware logic. recorder.py must NEVER follow them into
coding-plan-mode territory.
"""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

RECORDER_PATH = (
    Path(__file__).resolve().parents[3]
    / "src"
    / "task_center_runner"
    / "audit"
    / "recorder.py"
)

_FORBIDDEN_MODULE_PREFIXES: tuple[str, ...] = (
    "providers.clients.coding_plan",
)
_FORBIDDEN_MODULES_EXACT: frozenset[str] = frozenset(
    {
        "db.stores.model_store",
    }
)


def _module_is_forbidden(module: str | None) -> bool:
    if module is None:
        return False
    if module in _FORBIDDEN_MODULES_EXACT:
        return True
    for prefix in _FORBIDDEN_MODULE_PREFIXES:
        if module == prefix or module.startswith(prefix + "."):
            return True
    return False


def test_recorder_does_not_import_coding_plan_mode_packages() -> None:
    """recorder.py must not import any coding-plan-mode client module or model_store.

    Failure surface: token-leak via ``__repr__`` / ``asdict`` /
    str-coercion of a coding-plan-mode client object accidentally
    captured in a recorder record. See plan A8.
    """
    assert RECORDER_PATH.is_file(), (
        f"recorder.py not found at {RECORDER_PATH} — test path may need "
        "updating if the audit recorder moved."
    )

    source = RECORDER_PATH.read_text(encoding="utf-8")
    tree = ast.parse(source, filename=str(RECORDER_PATH))

    offenders: list[str] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom):
            module = node.module
            if _module_is_forbidden(module):
                offenders.append(
                    f"  recorder.py:{node.lineno}: from {module} import ..."
                )
        elif isinstance(node, ast.Import):
            for alias in node.names:
                if _module_is_forbidden(alias.name):
                    offenders.append(
                        f"  recorder.py:{node.lineno}: import {alias.name}"
                    )

    if offenders:
        pytest.fail(
            "Audit recorder must not import coding-plan-mode packages "
            "(A8 token-leak guard). Offenders:\n"
            + "\n".join(offenders)
            + "\n\nFix: move the offending import to a different module or "
            "restructure so the recorder accepts coding-plan-mode-related "
            "data only as opaque record dataclasses, never as live client "
            "objects."
        )
