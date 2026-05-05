"""pytest entry for the live_e2e_test suites.

- Opt-in by directory: ``pyproject.toml``'s ``norecursedirs`` keeps the
  default ``pytest backend/tests`` invocation from walking into this
  package. Run with ``pytest backend/tests/live_e2e_test``.
- Re-exports the shared fixtures from ``_harness/sandbox_fixture.py``.
- Enforces the import fence on the integrated suite: only files under
  ``layer_stack_overlay_occ/`` may import ``sandbox.api.tool``, and they
  must not reach into ``sandbox.layer_stack``, ``sandbox.overlay``, or
  ``sandbox.occ`` directly.
"""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

from ._harness.sandbox_fixture import (  # noqa: F401
    integrated_sandbox,
    layer_stack_sandbox,
    live_sandbox,
    occ_sandbox,
    overlay_sandbox,
)


SUITE_ROOT = Path(__file__).resolve().parent

_INTEGRATED_DIR = SUITE_ROOT / "layer_stack_overlay_occ"
_FORBIDDEN_FOR_INTEGRATED = (
    "sandbox.layer_stack",
    "sandbox.overlay",
    "sandbox.occ",
)
_PER_LAYER_FENCES: dict[str, tuple[str, ...]] = {
    "layer_stack": ("sandbox.overlay", "sandbox.occ", "sandbox.api.tool"),
    "overlay": ("sandbox.layer_stack", "sandbox.occ", "sandbox.api.tool"),
    "occ": ("sandbox.layer_stack", "sandbox.overlay", "sandbox.api.tool"),
}


pytestmark = [pytest.mark.live]


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers",
        "live: marks live Daytona-backed end-to-end tests (opt-in via env)",
    )


def _module_imports(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            names.add(node.module)
    return names


def _violations(imports: set[str], forbidden: tuple[str, ...]) -> list[str]:
    bad: list[str] = []
    for imported in imports:
        for prefix in forbidden:
            if imported == prefix or imported.startswith(f"{prefix}."):
                bad.append(imported)
                break
    return bad


def pytest_collection_modifyitems(
    config: pytest.Config, items: list[pytest.Item]
) -> None:
    """Fail any test whose module imports across the suite's import fence."""
    del config
    seen: dict[Path, list[str]] = {}
    for item in items:
        module_path = Path(getattr(item, "fspath", item.path))  # type: ignore[arg-type]
        if not module_path.is_file():
            continue
        try:
            relative = module_path.relative_to(SUITE_ROOT)
        except ValueError:
            continue
        if module_path in seen:
            offences = seen[module_path]
        else:
            imports = _module_imports(module_path)
            forbidden: tuple[str, ...] = ()
            top = relative.parts[0] if relative.parts else ""
            if top == "layer_stack_overlay_occ":
                forbidden = _FORBIDDEN_FOR_INTEGRATED
            elif top in _PER_LAYER_FENCES:
                forbidden = _PER_LAYER_FENCES[top]
            offences = _violations(imports, forbidden) if forbidden else []
            seen[module_path] = offences
        if offences:
            reason = (
                f"import-fence violation in {relative}: "
                f"forbidden imports {sorted(set(offences))}"
            )
            item.add_marker(pytest.mark.skip(reason=reason))
            # Also fail loudly: a fence violation is a hard error, not a skip.
            raise pytest.UsageError(reason)


# -- Load metric collector for the occ/ suite -----------------------------

import time as _time  # noqa: E402

from ._harness.occ_workload import LoadCollector, render_table  # noqa: E402


_CACHED_COLLECTOR: "LoadCollector | None" = None


@pytest.fixture(scope="session")
def occ_load_collector() -> LoadCollector:
    """Session-scoped collector for occ load-loop metrics.

    Records one JSONL row per iteration into
    ``.omc/results/live-e2e-occ-<utc>.jsonl``. ``pytest_sessionfinish``
    aggregates the rows into an ascii table printed under the
    ``== live-e2e-occ load metrics ==`` banner so ``pytest -s`` consumers
    can copy-paste the perf metrics into the README.
    """
    global _CACHED_COLLECTOR
    repo_root = SUITE_ROOT.parents[2]
    stamp = _time.strftime("%Y%m%dT%H%M%SZ", _time.gmtime())
    output = repo_root / ".omc" / "results" / f"live-e2e-occ-{stamp}.jsonl"
    collector = LoadCollector(output_path=output)
    _CACHED_COLLECTOR = collector
    return collector


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    del session, exitstatus
    collector = _CACHED_COLLECTOR
    if collector is None or not collector.stats:
        return
    rows = collector.summarize()
    print()
    print("== live-e2e-occ load metrics ==")
    print(f"jsonl: {collector.output_path}")
    print(render_table(rows))
