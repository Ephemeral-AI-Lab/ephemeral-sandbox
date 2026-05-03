"""Contract tests for ``sandbox/api/*`` — Step 1 cutover gate.

These tests enforce the Phase 1 dependency rule for the new API package:

* ``sandbox/api/*`` must not import ``sandbox.daytona.*`` or
  ``sandbox.runtime.*``.
* ``sandbox/api/*`` must not import ``daytona_sdk`` or anything under
  ``tools.*`` (a tools→api→tools cycle would defeat the layering).

The full import-fence test for ``tools/*`` and runtime internals
lands in Step 11; this is the narrow Step 1 gate that protects the
contract layer from regressing while later steps land.
"""

from __future__ import annotations

import ast
import inspect
from collections.abc import Iterator
from pathlib import Path

import pytest

from sandbox import api as sandbox_api
from sandbox.api import RequestActor, SandboxApi, SandboxTransport


_API_ROOT = Path(sandbox_api.__file__).parent
_FORBIDDEN_PREFIXES: tuple[str, ...] = (
    "sandbox.daytona",
    "sandbox.runtime",
    "daytona_sdk",
    "tools.",
)
# Modules in ``sandbox/api/`` that are the engine bridge are allowed to
# import engine spec types from ``sandbox.occ.types`` and
# ``sandbox.occ.patching.patcher``. Everything else under
# ``sandbox.runtime`` (services, mutations engine internals,
# overlay) stays forbidden.
_BRIDGE_MODULES: frozenset[str] = frozenset({"audit.py"})
_BRIDGE_ALLOWED: tuple[str, ...] = (
    "sandbox.occ.types",
    "sandbox.occ.patching.patcher",
)


def _iter_api_modules() -> Iterator[Path]:
    for path in sorted(_API_ROOT.rglob("*.py")):
        if "__pycache__" in path.parts:
            continue
        yield path


def _imported_modules(source: str) -> set[str]:
    tree = ast.parse(source)
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                names.add(alias.name)
        elif isinstance(node, ast.ImportFrom):
            if node.module:
                names.add(node.module)
    return names


@pytest.mark.parametrize("module_path", list(_iter_api_modules()))
def test_no_provider_or_engine_imports(module_path: Path) -> None:
    """``sandbox/api/*`` must stay free of provider and engine couplings.

    Engine-bridge modules (currently ``audit.py``) are allowed to import
    the narrow set of engine spec types listed in ``_BRIDGE_ALLOWED`` so
    they can translate API request shapes into the engine's spec types.
    Everything else under ``sandbox.runtime`` stays forbidden.
    """
    is_bridge = module_path.name in _BRIDGE_MODULES
    source = module_path.read_text(encoding="utf-8")
    for name in _imported_modules(source):
        if is_bridge and name in _BRIDGE_ALLOWED:
            continue
        for forbidden in _FORBIDDEN_PREFIXES:
            assert not (name == forbidden.rstrip(".") or name.startswith(forbidden)), (
                f"{module_path.name} imports forbidden module {name!r} "
                f"(matches prefix {forbidden!r})"
            )


def test_protocols_declare_methods() -> None:
    """The contract Protocols expose at least one async method.

    Catches regressions where a Protocol body is accidentally emptied or
    replaced with a stub that can't actually be implemented.
    """
    for proto in (SandboxApi, SandboxTransport):
        members = [
            name
            for name, fn in inspect.getmembers(proto, predicate=inspect.isfunction)
            if not name.startswith("_")
        ]
        assert members, f"{proto.__name__} has no public methods declared"


def test_request_actor_defaults() -> None:
    """``RequestActor`` keeps optional run/agent/task ids as empty strings.

    The Phase 1 plan adopts these defaults (today's ``AgentAttribution``
    has none) so call sites that only know the agent id can still
    construct a valid actor without threading sentinels.
    """
    actor = RequestActor(agent_id="worker-1")
    assert actor.agent_id == "worker-1"
    assert actor.run_id == ""
    assert actor.agent_run_id == ""
    assert actor.task_id == ""


def test_request_actor_is_immutable() -> None:
    """``RequestActor`` is frozen — accidental mutation is a programming error."""
    actor = RequestActor(agent_id="a")
    with pytest.raises((AttributeError, TypeError)):
        actor.agent_id = "b"  # type: ignore[misc]


# -- Step 2 — audit/attribution shim wiring ---------------------------------


def test_attribution_round_trip() -> None:
    """``RequestActor`` <-> ``AgentAttribution`` round-trip preserves all fields."""
    from sandbox.api.attribution import (
        AgentAttribution,
        actor_from_attribution,
        attribution_from_actor,
    )

    original = AgentAttribution(
        agent_id="alice",
        run_id="r-1",
        agent_run_id="ar-1",
        task_id="t-1",
    )
    actor = actor_from_attribution(original)
    assert actor == RequestActor(
        agent_id="alice", run_id="r-1", agent_run_id="ar-1", task_id="t-1",
    )
    assert attribution_from_actor(actor) == original


def test_build_actor_priority_matches_legacy_resolver() -> None:
    """``build_actor`` keeps the legacy preferred → agent_run_id → agent_id priority."""
    from sandbox.api.attribution import build_actor

    explicit = build_actor(
        agent_id="raw", agent_run_id="ar-1", preferred_agent_id="explicit",
    )
    assert explicit.agent_id == "explicit"

    by_run = build_actor(agent_id="raw", agent_run_id="ar-1")
    assert by_run.agent_id == "ar-1"

    by_raw = build_actor(agent_id="raw")
    assert by_raw.agent_id == "raw"


def test_audit_module_re_exports_engine_helpers() -> None:
    """``sandbox.api.audit`` re-exports the engine helpers tools rely on.

    Locks the public surface so the tools/core/sandbox_commit.py shim
    keeps compiling: it does ``from sandbox.api.audit import (CommitOp,
    FileChangeResult, commit_metadata, submit_commit,
    submit_shell_cmd)``.
    """
    from sandbox.api import audit

    for name in (
        "CommitOp",
        "FileChangeResult",
        "commit_metadata",
        "submit_commit",
        "submit_shell_cmd",
    ):
        assert hasattr(audit, name), f"sandbox.api.audit missing {name!r}"


def test_legacy_tool_core_shims_are_deleted() -> None:
    """Step 10 removes the temporary tools/core compatibility shims."""
    import importlib.util

    for module_name in (
        "tools.core.ci_adapter",
        "tools.core.ci_attribution",
        "tools.core.sandbox_commit",
    ):
        assert importlib.util.find_spec(module_name) is None
