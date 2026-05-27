"""Tests for the strict :func:`benchmarks.sweevo.prompt.load_pr_description`.

The lenient loader (``pr_description_for_instance`` /
``load_pr_description_overrides``) silently returns ``""`` / ``{}`` on
missing data. The strict variant fails loudly so the CSV benchmarker can
short-circuit before sandbox creation.
"""

from __future__ import annotations

import textwrap
from pathlib import Path

import pytest

from task_center_runner.benchmarks.sweevo.setup import (
    load_pr_description,
    load_pr_description_overrides,
)


@pytest.fixture(autouse=True)
def _clear_lru_cache() -> None:
    """The strict loader shares the lenient loader's LRU cache (maxsize=8).

    Tests that monkeypatch the CSV path or write throwaway CSVs would
    otherwise see stale ``{}`` results from previous test runs. Clear
    before AND after each test so the autoflushing is symmetric.
    """
    load_pr_description_overrides.cache_clear()
    yield
    load_pr_description_overrides.cache_clear()


def _write_csv(tmp_path: Path, rows: list[tuple[str, str]]) -> Path:
    """Write a SWE-EVO PR-description CSV with ``test_folder,info_log_path,pr_description``."""
    path = tmp_path / "pr_descriptions.csv"
    lines = ["test_folder,info_log_path,pr_description"]
    for instance_id, description in rows:
        escaped = description.replace('"', '""')
        lines.append(f'{instance_id},/dev/null,"{escaped}"')
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


def test_strict_loader_happy_path(tmp_path: Path) -> None:
    csv_path = _write_csv(tmp_path, [("inst_a", "Fix the bug")])

    result = load_pr_description("inst_a", csv_path=csv_path)

    assert result == "Fix the bug"


def test_strict_loader_missing_file(tmp_path: Path) -> None:
    missing = tmp_path / "does_not_exist.csv"

    with pytest.raises(FileNotFoundError) as exc_info:
        load_pr_description("inst_a", csv_path=missing)

    assert str(missing) in str(exc_info.value)


def test_strict_loader_missing_row(tmp_path: Path) -> None:
    csv_path = _write_csv(tmp_path, [("inst_a", "Fix the bug")])

    with pytest.raises(KeyError) as exc_info:
        load_pr_description("inst_b", csv_path=csv_path)

    message = str(exc_info.value)
    assert "inst_b" in message
    assert str(csv_path) in message


def test_strict_loader_empty_value(tmp_path: Path) -> None:
    # Empty string in the CSV column.
    csv_path = tmp_path / "pr.csv"
    csv_path.write_text(
        "test_folder,info_log_path,pr_description\n"
        'inst_empty,/dev/null,""\n',
        encoding="utf-8",
    )

    with pytest.raises(ValueError) as exc_info:
        load_pr_description("inst_empty", csv_path=csv_path)

    assert "inst_empty" in str(exc_info.value)


def test_strict_loader_whitespace_only_value(tmp_path: Path) -> None:
    csv_path = tmp_path / "pr.csv"
    csv_path.write_text(
        "test_folder,info_log_path,pr_description\n"
        'inst_ws,/dev/null,"   \n\t  "\n',
        encoding="utf-8",
    )

    with pytest.raises(ValueError):
        load_pr_description("inst_ws", csv_path=csv_path)


def test_strict_loader_preserves_multiline(tmp_path: Path) -> None:
    multiline = textwrap.dedent(
        """\
        - First bullet
        - Second bullet
          with a continuation
        """
    ).rstrip("\n")
    csv_path = _write_csv(tmp_path, [("inst_multi", multiline)])

    result = load_pr_description("inst_multi", csv_path=csv_path)

    assert result == multiline
    assert "\n" in result  # multi-line preserved


def test_strict_loader_shares_lru_cache(tmp_path: Path) -> None:
    """Strict loader must call the cached ``load_pr_description_overrides``.

    Verify by populating the cache via the lenient helper, then deleting
    the CSV. The strict loader's pre-existence check should fail (CSV
    gone), proving it does NOT just trust the cache.
    """
    csv_path = _write_csv(tmp_path, [("inst_a", "alpha")])

    # Warm the cache via the lenient loader.
    cached = load_pr_description_overrides(str(csv_path))
    assert cached == {"inst_a": "alpha"}

    # Sanity: strict loader returns the cached value while the file exists.
    assert load_pr_description("inst_a", csv_path=csv_path) == "alpha"

    # Cache HIT — but file is gone. Strict loader must fail-fast.
    csv_path.unlink()
    with pytest.raises(FileNotFoundError):
        load_pr_description("inst_a", csv_path=csv_path)


def test_strict_loader_env_var_fallback(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Strict loader resolves via SWEEVO_PR_DESCRIPTIONS_CSV when csv_path is None."""
    csv_path = _write_csv(tmp_path, [("inst_env", "from env")])
    monkeypatch.setenv("SWEEVO_PR_DESCRIPTIONS_CSV", str(csv_path))

    assert load_pr_description("inst_env") == "from env"
