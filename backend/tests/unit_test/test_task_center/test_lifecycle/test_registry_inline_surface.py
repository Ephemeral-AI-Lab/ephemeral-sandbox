"""Phase 5i regression test — Registry inline surface (lever #5).

After deleting task_center/registry.py and inlining the 5 classmethod
contract into PredicateRegistry and RecipeRegistry directly, pin the
public surface of both registries so future edits cannot drop methods
or change signatures silently.

Plan: .omc/plans/task-center-folder-reframe-20260514.md (lever #5)
"""

from __future__ import annotations

import inspect

from task_center._core.agent_routing import PredicateRegistry
from task_center.context_engine.recipes_registry import (
    RecipeRegistry,
)


_EXPECTED_SURFACE = {"register", "get", "has", "list_ids", "clear"}


def test_predicate_registry_public_surface_preserved() -> None:
    public = {
        name for name in vars(PredicateRegistry) if not name.startswith("_")
    }
    missing = _EXPECTED_SURFACE - public
    assert not missing, f"PredicateRegistry missing methods: {missing}"


def test_recipe_registry_public_surface_preserved() -> None:
    public = {name for name in vars(RecipeRegistry) if not name.startswith("_")}
    missing = _EXPECTED_SURFACE - public
    assert not missing, f"RecipeRegistry missing methods: {missing}"


def test_registry_get_raises_typed_error() -> None:
    PredicateRegistry.clear()
    try:
        PredicateRegistry.get("nope")
    except KeyError as exc:
        assert "nope" in str(exc)
    else:
        raise AssertionError("expected KeyError")

    from task_center.context_engine.core import ContextEngineError

    RecipeRegistry.clear()
    try:
        RecipeRegistry.get("nope")
    except ContextEngineError as exc:
        assert "nope" in str(exc)
    else:
        raise AssertionError("expected ContextEngineError")


def test_registry_register_signatures_distinct() -> None:
    # Predicate registers by (name, fn); recipe registers by single payload.
    p_sig = inspect.signature(PredicateRegistry.register)
    assert list(p_sig.parameters) == ["name", "fn"]
    r_sig = inspect.signature(RecipeRegistry.register)
    assert list(r_sig.parameters) == ["recipe"]


def test_old_registry_module_is_gone() -> None:
    import importlib

    try:
        importlib.import_module("task_center.registry")
    except ModuleNotFoundError:
        return
    raise AssertionError("task_center.registry should have been deleted")
