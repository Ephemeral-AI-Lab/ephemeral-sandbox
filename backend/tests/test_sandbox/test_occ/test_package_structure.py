"""OCC package boundary tests."""

from __future__ import annotations

from pathlib import Path

import sandbox.occ


def _occ_root() -> Path:
    return Path(sandbox.occ.__file__).resolve().parent


def test_occ_root_contains_only_entrypoints_and_subpackages() -> None:
    expected = {
        "__init__.py",
        "changeset",
        "client.py",
        "commit_transaction.py",
        "content",
        "direct",
        "gated",
        "orchestrator.py",
        "runtime_ops.py",
        "service.py",
    }

    ignored = {"__pycache__", ".DS_Store"}
    actual = {
        path.name
        for path in _occ_root().iterdir()
        if path.name not in ignored and not _generated_only_dir(path)
    }

    assert actual == expected


def test_occ_does_not_import_code_intelligence_or_overlay() -> None:
    forbidden = ("sandbox.overlay",)
    hits: list[tuple[Path, str]] = []
    for path in _occ_root().rglob("*.py"):
        text = path.read_text(encoding="utf-8")
        for token in forbidden:
            if token in text:
                hits.append((path.relative_to(_occ_root()), token))

    assert hits == []


def _generated_only_dir(path: Path) -> bool:
    if not path.is_dir():
        return False
    return all(child.name in {"__pycache__", ".DS_Store"} for child in path.iterdir())
