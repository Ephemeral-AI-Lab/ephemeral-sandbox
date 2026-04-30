"""Phase 05 regression tests for removed legacy harness artifacts."""

from __future__ import annotations

from dataclasses import fields
from pathlib import Path
import re
from typing import Callable

from task_center.complex_task.request import ComplexTaskRequest


REPO_ROOT = Path(__file__).resolve().parents[4]
SRC_ROOT = REPO_ROOT / "backend" / "src"
ROOT_TOKEN_RE = re.compile(r"(?<![A-Za-z0-9_])ROOT(?![A-Za-z0-9_])")


def _source_files() -> tuple[Path, ...]:
    return tuple(sorted(SRC_ROOT.rglob("*.py")))


def _matching_source_files(predicate: Callable[[str], bool]) -> list[str]:
    matches: list[str] = []
    for path in _source_files():
        text = path.read_text(encoding="utf-8")
        if predicate(text):
            matches.append(str(path.relative_to(REPO_ROOT)))
    return matches


def test_no_submit_request_plan_anywhere_in_src():
    matches = _matching_source_files(lambda text: "submit_request_plan" in text)

    assert matches == []


def test_no_retry_on_failure_constant_in_src():
    matches = _matching_source_files(lambda text: "RETRY_ON_FAILURE" in text)

    assert matches == []


def test_no_retry_after_partial_in_src():
    matches = _matching_source_files(lambda text: "retry_after_partial" in text)

    assert matches == []


def test_no_root_spawn_or_creation_reason_in_src():
    matches = _matching_source_files(
        lambda text: ROOT_TOKEN_RE.search(text) is not None
    )

    assert matches == []


def test_complex_task_request_has_no_retry_budget_field():
    field_names = {field.name for field in fields(ComplexTaskRequest)}

    assert field_names.isdisjoint({"retry_budget", "attempt_budget", "max_retries"})
