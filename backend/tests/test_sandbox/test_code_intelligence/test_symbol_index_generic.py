"""Tests for Python-only symbol indexing."""

from __future__ import annotations

from sandbox.code_intelligence.indexing.symbol_index import SymbolIndex


def test_symbol_index_ignores_non_python_symbols(tmp_path) -> None:
    content = "export class Example {}\n"
    file_path = tmp_path / "sample.ts"
    file_path.write_text(content, encoding="utf-8")

    index = SymbolIndex(str(tmp_path))

    generation = index.refresh(str(file_path), content)
    assert generation > 0

    symbols = index.file_symbols(str(file_path))

    assert symbols == []


def test_symbol_index_paths_with_prefix_preserves_dot_directories(tmp_path) -> None:
    hidden = tmp_path / ".config" / "settings.py"
    hidden.parent.mkdir()
    content = "VALUE = 1\n"
    hidden.write_text(content, encoding="utf-8")

    index = SymbolIndex(str(tmp_path))
    index.refresh(str(hidden), content)

    assert index.paths_with_prefix(".config") == [str(hidden)]
