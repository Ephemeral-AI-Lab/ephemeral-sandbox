"""Agent definition loading from Markdown files with YAML frontmatter."""

from __future__ import annotations

import logging
from collections.abc import Iterable
from pathlib import Path

from pydantic import ValidationError

from config.markdown import parse_markdown_frontmatter

from .model import AgentDefinition

logger = logging.getLogger(__name__)


# Profiles whose ``system_prompt`` is prepended with
# ``_main_role_contract.md``. Path-based: every ``.md`` directly under
# ``agents/profile/main/`` that is not a ``_*.md`` private include.
_MAIN_PROFILE_DIRNAME = "main"
_MAIN_ROLE_CONTRACT_NAME = "_main_role_contract.md"


def _main_role_contract_text(profile_path: Path) -> str | None:
    """Return the contract markdown body for an in-harness main profile.

    Returns ``None`` when the profile is not in scope (not under ``main/``,
    is itself a ``_*.md`` private include, or the contract file is missing).
    """
    if profile_path.parent.name != _MAIN_PROFILE_DIRNAME:
        return None
    if profile_path.name.startswith("_"):
        return None
    contract_path = profile_path.parent / _MAIN_ROLE_CONTRACT_NAME
    if not contract_path.is_file():
        return None
    return contract_path.read_text(encoding="utf-8").rstrip()


def _load_agent_files(paths: Iterable[Path]) -> list[AgentDefinition]:
    agents: list[AgentDefinition] = []
    for path in sorted(paths):
        if path.name.startswith("_"):
            # ``_*.md`` are private includes (e.g. ``_main_role_contract.md``)
            # — not standalone agent profiles. Skip them.
            continue
        try:
            fm, body = parse_markdown_frontmatter(path.read_text(encoding="utf-8"))
        except OSError:
            logger.error("Could not read agent definition %s", path, exc_info=True)
            raise
        data = dict(fm)
        if not data.get("name"):
            data["name"] = path.stem
        data["description"] = str(data.get("description") or f"Agent: {data['name']}")
        contract = _main_role_contract_text(path)
        if contract is not None and body:
            data["system_prompt"] = f"{contract}\n\n{body}"
        elif contract is not None:
            data["system_prompt"] = contract
        elif body:
            data["system_prompt"] = body
        if "agent_kind" not in data:
            raise ValueError(
                f"Agent profile {path} is missing required 'agent_kind:' "
                "frontmatter field. Declare one of planner / executor / verifier / "
                "evaluator / advisor / explorer."
            )
        skill_value = data.get("skill")
        if skill_value:
            skill_path = (path.parent / str(skill_value)).resolve()
            if not skill_path.is_file():
                raise FileNotFoundError(
                    f"Agent profile {path} declares skill: {skill_value!r}, "
                    f"but {skill_path} does not exist."
                )
            data["skill"] = skill_path
        try:
            agents.append(AgentDefinition.model_validate(data))
        except ValidationError:
            logger.error("Invalid agent definition in %s", path, exc_info=True)
            raise
    return agents


def load_agents_dir(directory: Path) -> list[AgentDefinition]:
    """Load agent definitions from .md files directly in *directory*."""
    if not directory.is_dir():
        return []
    return _load_agent_files(directory.glob("*.md"))


def load_agents_tree(directory: Path) -> list[AgentDefinition]:
    """Load agent definitions from all .md files under *directory*."""
    if not directory.is_dir():
        return []
    return _load_agent_files(directory.rglob("*.md"))
