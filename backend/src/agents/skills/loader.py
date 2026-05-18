"""Startup lint over ``AgentDefinition.skill`` files.

A skill body is row 4 at launch (`task_center/context_engine/core.py:
build_skill_message`). Row 3 (the terminal catalog) owns terminal-tool
authority; the skill must stay terminal-silent at the contract level so
the two rows never drift. This module enforces that floor at process
start by scanning every declared skill file for:

* ``submit_*`` substrings (any terminal-tool name pattern)
* any key from ``TERMINAL_DESCRIPTORS`` as a substring (full or partial
  terminal name)

Bridging language (e.g. "the decision point", "the submission step") is
permitted — those phrases anticipate the row-3 catalog without restating
selection rules. Substring/key matches signal a contract-level mention
and raise :class:`SkillLintError`.

The scan runs once at app startup from
:func:`agents.validate_agent_definitions_resolved`, with all definitions
already in hand; no duplicate scan path exists in
``agents/definition/loader.py``.
"""

from __future__ import annotations

import re
from collections.abc import Iterable
from pathlib import Path

from agents.definition.model import AgentDefinition
from config.markdown import parse_markdown_frontmatter

# Matches any ``submit_<identifier>`` token. Captures terminal-tool names
# even when authors invent ones not in ``TERMINAL_DESCRIPTORS`` yet.
_SUBMIT_PATTERN = re.compile(r"submit_[A-Za-z0-9_]+")


class SkillLintError(ValueError):
    """Raised when a skill file violates the terminal-silence contract."""


def scan_skill_file(path: Path) -> list[str]:
    """Return the list of lint violations for one skill file.

    Empty list means the file is clean. Non-empty list means each entry
    is a human-readable description of one violation. Frontmatter is
    stripped before scanning so author metadata cannot trigger false
    positives.
    """
    # Lazy import: ``tools._terminals.registry`` imports pydantic and
    # downstream tool modules. Keeping this lazy lets test fixtures
    # exercise the lint scan without pulling in the full tool surface.
    from tools._terminals.registry import TERMINAL_DESCRIPTORS

    raw = path.read_text(encoding="utf-8")
    _, body = parse_markdown_frontmatter(raw)
    violations: list[str] = []

    submit_hits = sorted(set(_SUBMIT_PATTERN.findall(body)))
    for hit in submit_hits:
        violations.append(
            f"skill body mentions terminal-tool name {hit!r}; row 4 must be "
            "terminal-silent (row 3 owns the catalog)"
        )

    # Catch ``TERMINAL_DESCRIPTORS`` keys that escape the ``submit_*``
    # pattern (none today, but the registry could grow non-submit terminals).
    for key in TERMINAL_DESCRIPTORS:
        if key in submit_hits:
            continue
        if key in body:
            violations.append(
                f"skill body mentions TERMINAL_DESCRIPTORS key {key!r}; row 4 "
                "must be terminal-silent (row 3 owns the catalog)"
            )

    return violations


def validate_skill_files(definitions: Iterable[AgentDefinition]) -> None:
    """Scan every declared ``AgentDefinition.skill`` and raise on violation.

    Called from :func:`agents.validate_agent_definitions_resolved` after
    all profiles are registered. A single ``SkillLintError`` is raised
    listing every violation across every skill file so authors see the
    full picture in one pass.
    """
    all_violations: list[str] = []
    for definition in definitions:
        if definition.skill is None:
            continue
        for violation in scan_skill_file(definition.skill):
            all_violations.append(
                f"{definition.name!r} (skill={definition.skill}): {violation}"
            )

    if all_violations:
        raise SkillLintError(
            "Skill-file lint failed:\n  - "
            + "\n  - ".join(all_violations)
        )
