"""Phase 4c invariant — ``task_center_runner.core.*`` is runner-agnostic.

The unified engine must not reach into the mock-runner internals or any
specific benchmark adapter. The test enforces two layers of insulation:

1. Module-graph: source files under ``core/`` MUST NOT import
   ``MockSquadRunner``, ``MutableMockState``, anything under
   ``task_center_runner.agent.mock`` (or its legacy alias
   ``task_center_runner.squad``), or anything under
   ``task_center_runner.benchmarks.sweevo``.

2. Source-string: each ``core/*.py`` source MUST NOT contain
   ``hasattr(``, ``getattr(runner``, ``isinstance(runner``,
   ``collect_extras``, or ``runner_extras``. These are the canonical
   Python escape valves that would re-introduce a runner-shape
   assumption; the import-graph layer already blocks symbol-level reach
   to ``MockSquadRunner`` but a reviewer needs the belt-and-suspenders.
"""

from __future__ import annotations

import ast
import re
import tokenize
from pathlib import Path

CORE_DIR = (
    Path(__file__).resolve().parents[3]
    / "src"
    / "task_center_runner"
    / "core"
)

_FORBIDDEN_IMPORT_TOKENS = (
    "MockSquadRunner",
    "MutableMockState",
    "from task_center_runner.squad",
    "import task_center_runner.squad",
    "from task_center_runner.agent.mock",
    "import task_center_runner.agent.mock",
    "from task_center_runner.benchmarks.sweevo",
    "import task_center_runner.benchmarks.sweevo",
)

_FORBIDDEN_SOURCE_PATTERNS = (
    r"\bhasattr\(",
    r"\bgetattr\(\s*runner\b",
    r"\bisinstance\(\s*runner\b",
    r"\bcollect_extras\b",
    r"\brunner_extras\b",
)


def _core_python_files() -> list[Path]:
    # fixtures.py is test infrastructure, not engine code — exempt.
    return sorted(p for p in CORE_DIR.rglob("*.py") if p.name != "fixtures.py")


def _strip_comments_and_docstrings(source: str) -> str:
    """Return source text with comments and string literals removed.

    The forbidden-tokens check should ignore mentions inside docstrings
    and comments — those describe the runner-agnostic property, they
    don't violate it. We rebuild the source from tokens, dropping
    ``COMMENT`` and ``STRING``-typed ones.
    """
    try:
        module = ast.parse(source)
    except SyntaxError:
        return source
    # Build a set of byte ranges to blank: every Expr-statement whose value
    # is a Str (docstring) plus every STRING token via tokenize.
    lines = source.splitlines(keepends=True)
    keep: list[str] = []
    for tok in tokenize.generate_tokens(iter(lines).__next__):
        if tok.type in (tokenize.COMMENT, tokenize.STRING):
            continue
        keep.append(tok.string)
    _ = module  # silence: ast parse just validates syntax
    return " ".join(keep)


def test_core_has_no_forbidden_imports() -> None:
    offenders: list[tuple[Path, str]] = []
    for source_path in _core_python_files():
        text = _strip_comments_and_docstrings(source_path.read_text(encoding="utf-8"))
        for token in _FORBIDDEN_IMPORT_TOKENS:
            if token in text:
                offenders.append((source_path.relative_to(CORE_DIR), token))
    assert not offenders, (
        "Forbidden runner-specific imports leaked into task_center_runner/core/:\n"
        + "\n".join(f"  {path} contains {token!r}" for path, token in offenders)
    )


def test_core_source_has_no_runner_shape_escape_valves() -> None:
    offenders: list[tuple[Path, str]] = []
    for source_path in _core_python_files():
        text = _strip_comments_and_docstrings(source_path.read_text(encoding="utf-8"))
        for pattern in _FORBIDDEN_SOURCE_PATTERNS:
            if re.search(pattern, text):
                offenders.append((source_path.relative_to(CORE_DIR), pattern))
    assert not offenders, (
        "Forbidden runner-shape escape valves found in task_center_runner/core/:\n"
        + "\n".join(f"  {path} matches /{pattern}/" for path, pattern in offenders)
    )


_BACKEND_ROOT = Path(__file__).resolve().parents[3]
_FORBIDDEN_BENCHMARKS_PATTERNS = (
    re.compile(r"\bfrom\s+benchmarks\."),
    re.compile(r"\bimport\s+benchmarks\."),
    re.compile(r"\bimport\s+benchmarks\s+as\b"),
    re.compile(r"\bimport\s+benchmarks\s*$", re.MULTILINE),
)


def test_no_legacy_benchmarks_imports_anywhere() -> None:
    """Per migration acceptance criterion 12 — no ``from benchmarks.`` anywhere.

    The legacy ``backend/src/benchmarks/`` package was deleted in favor of
    ``task_center_runner.benchmarks.sweevo``; any remaining reference is a
    leak from before the rename and will ImportError at runtime.

    Patterns are matched after stripping comments and string literals so
    that documentation in tests/docs describing the migration does not
    register as a violation.
    """
    src_root = _BACKEND_ROOT / "src"
    tests_root = _BACKEND_ROOT / "tests"
    offenders: list[tuple[Path, str]] = []
    self_path = Path(__file__).resolve()
    for root in (src_root, tests_root):
        if not root.exists():
            continue
        for path in root.rglob("*.py"):
            if "__pycache__" in path.parts:
                continue
            if path.resolve() == self_path:
                continue
            text = _strip_comments_and_docstrings(
                path.read_text(encoding="utf-8", errors="replace")
            )
            for pat in _FORBIDDEN_BENCHMARKS_PATTERNS:
                if pat.search(text):
                    offenders.append((path.relative_to(_BACKEND_ROOT), pat.pattern))
                    break
    assert not offenders, (
        "Legacy `benchmarks.*` imports still present (use "
        "`task_center_runner.benchmarks.sweevo.*` instead):\n"
        + "\n".join(f"  {path} matches /{pattern}/" for path, pattern in offenders)
    )
