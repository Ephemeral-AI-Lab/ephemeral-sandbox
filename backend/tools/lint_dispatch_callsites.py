"""Phase 4 §AC10/§G1: callsite lint for daemon dispatch + plugin gate.

Two invariants matter for the per-agent quiesce primitive to stay sound:

1. ``dispatch_workspace_tool_call`` must be reached only from the daemon
   RPC layer (``backend/src/sandbox/daemon/``). A new caller anywhere
   else would bypass the entry_lock-guarded
   :func:`acquire_dispatch_slot` wrapping and silently re-open Phase 4's
   D1 race.
2. ``_plugin_block_decision`` (the renamed Phase 4 plugin gate) must
   have exactly one caller —
   ``backend/src/sandbox/daemon/rpc/dispatcher.py``. A second caller
   would have to re-establish the dispatch-slot contract independently
   to stay safe; pending review, the lint blocks the addition.

The script is invoked from ``make lint`` and exits non-zero on
violation. Test files and the defining files themselves are exempt.

Usage::

    python -m backend.tools.lint_dispatch_callsites [--repo-root PATH]
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Iterable

REPO_RELATIVE_SRC = Path("backend/src")
REPO_RELATIVE_TESTS = Path("backend/tests")

# (symbol_name, allowed_source_roots, defining_file)
_RULES = (
    (
        "dispatch_workspace_tool_call",
        (Path("backend/src/sandbox/daemon"),),
        Path("backend/src/sandbox/daemon/workspace_tool_dispatch.py"),
    ),
    (
        "_plugin_block_decision",
        (Path("backend/src/sandbox/daemon/rpc"),),
        Path("backend/src/sandbox/daemon/rpc/dispatcher.py"),
    ),
)

_PY_FILES_GLOBS = ("**/*.py",)


def _iter_python_files(roots: Iterable[Path]) -> list[Path]:
    files: list[Path] = []
    for root in roots:
        if not root.exists():
            continue
        for pattern in _PY_FILES_GLOBS:
            files.extend(root.rglob(pattern))
    return files


def _is_under(path: Path, roots: tuple[Path, ...]) -> bool:
    try:
        resolved = path.resolve()
    except OSError:
        return False
    for root in roots:
        try:
            resolved.relative_to(root.resolve())
        except ValueError:
            continue
        return True
    return False


def _is_test_file(path: Path, repo_root: Path) -> bool:
    """Test-file detection scoped to the repo_root.

    Walking the full ``path.parts`` would false-positive on ``pytest`` tmp
    paths (``/private/var/.../pytest-of-yifanxu/test_foo_0/``), so the
    decision must use only the path's location inside ``repo_root``.
    """
    try:
        rel = path.resolve().relative_to(repo_root.resolve())
    except ValueError:
        return False
    parts = rel.parts
    return "tests" in parts or any(part.startswith("test_") for part in parts)


def _scan_callers(symbol: str, files: Iterable[Path]) -> list[tuple[Path, int]]:
    callers: list[tuple[Path, int]] = []
    # Match either ``symbol(`` or ``symbol,`` (as a re-export) but not
    # ``def symbol`` or ``class symbol``. Keep the regex deliberately
    # loose — any usage that pulls the name out of the module counts.
    pattern = re.compile(rf"(?<![A-Za-z0-9_]){re.escape(symbol)}(?![A-Za-z0-9_])")
    for path in files:
        try:
            text = path.read_text(encoding="utf-8")
        except OSError:
            continue
        for lineno, line in enumerate(text.splitlines(), start=1):
            stripped = line.lstrip()
            if stripped.startswith(("def ", "class ", "#")):
                continue
            if pattern.search(line):
                callers.append((path, lineno))
    return callers


def lint(repo_root: Path) -> list[str]:
    """Return human-readable violation strings; empty list = pass."""
    violations: list[str] = []
    src_root = repo_root / REPO_RELATIVE_SRC
    tests_root = repo_root / REPO_RELATIVE_TESTS
    files = _iter_python_files((src_root, tests_root))
    for symbol, allowed_roots, defining_file in _RULES:
        allowed = tuple(repo_root / root for root in allowed_roots)
        defining = repo_root / defining_file
        external_callers: list[tuple[Path, int]] = []
        for path, lineno in _scan_callers(symbol, files):
            if path.resolve() == defining.resolve():
                continue
            if _is_test_file(path, repo_root):
                continue
            if _is_under(path, allowed):
                continue
            external_callers.append((path, lineno))
        if external_callers:
            location_lines = "\n".join(
                f"    {path.relative_to(repo_root)}:{lineno}"
                for path, lineno in external_callers
            )
            allowed_lines = ", ".join(str(root) for root in allowed_roots)
            violations.append(
                f"[lint_dispatch_callsites] '{symbol}' has unexpected "
                f"callers outside {allowed_lines!s}:\n{location_lines}"
            )
    return violations


def _default_repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=_default_repo_root(),
        help="Repository root (defaults to two levels above this file).",
    )
    args = parser.parse_args(argv)
    violations = lint(args.repo_root)
    if violations:
        for violation in violations:
            print(violation, file=sys.stderr)
        return 1
    print("lint_dispatch_callsites: ok")
    return 0


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
