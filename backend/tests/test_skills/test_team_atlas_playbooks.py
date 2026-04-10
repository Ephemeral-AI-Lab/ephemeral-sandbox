"""Regression tests for Atlas role playbooks."""

from __future__ import annotations

from pathlib import Path


_BACKEND_ROOT = Path(__file__).resolve().parents[2]
_BUILDER_PLAYBOOK = (
    _BACKEND_ROOT / "src/skills/bundled/content/team-atlas-builder-playbook/SKILL.md"
)
_REFRESHER_PLAYBOOK = (
    _BACKEND_ROOT / "src/skills/bundled/content/team-atlas-refresher-playbook/SKILL.md"
)


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def test_builder_playbook_allows_background_join_tools() -> None:
    content = _read(_BUILDER_PLAYBOOK)

    assert "- `check_background_progress(task_id=..., ...)`" in content
    assert "- `wait_for_background_task(task_id=..., ...)`" in content
    assert (
        "Only `ci_workspace_structure`, `run_subagent`, "
        "`check_background_progress`, and `wait_for_background_task`."
    ) in content


def test_builder_playbook_persists_empty_subsystems() -> None:
    content = _read(_BUILDER_PLAYBOOK)

    assert "or omit it entirely" not in content
    assert "so the atlas records that the subsystem is empty" in content
    assert "A zero-coverage, no-subdivision scout result is still a real atlas chunk" in content


def test_refresher_playbook_hard_rules_match_whitelist() -> None:
    content = _read(_REFRESHER_PLAYBOOK)

    assert "- `check_background_progress(task_id=..., ...)`" in content
    assert "- `wait_for_background_task(task_id=..., ...)`" in content
    assert (
        "3. **Whitelist enforced.** Only `run_subagent`, `check_background_progress`, "
        "and `wait_for_background_task`."
    ) in content
