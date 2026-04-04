"""Validation service for agent definitions."""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

from ephemeralos.agents.types import EFFORT_LEVELS
from ephemeralos.agents.api.schemas import AgentValidationResult

if TYPE_CHECKING:
    from ephemeralos.agents.api.schemas import AgentDefinitionCreate, AgentDefinitionUpdate
    from ephemeralos.tools.base import ToolRegistry

logger = logging.getLogger(__name__)


class AgentDefinitionValidator:
    """Validates that agent definition references are resolvable."""

    def __init__(self, tool_registry: "ToolRegistry | None") -> None:
        self._tool_registry = tool_registry

    def validate(self, defn: "AgentDefinitionCreate | AgentDefinitionUpdate") -> AgentValidationResult:
        errors: list[str] = []
        warnings: list[str] = []

        toolkits = getattr(defn, "toolkits", None)
        if toolkits:
            known: set[str] = set()
            if self._tool_registry:
                known = {tk.name for tk in self._tool_registry.list_toolkits()}
            from ephemeralos.tools.factory import has_factory  # noqa: PLC0415
            for tk in toolkits:
                if tk not in known and not has_factory(tk):
                    errors.append(f"Unknown toolkit: {tk}")

        effort = getattr(defn, "effort", None)
        if effort is not None and effort not in EFFORT_LEVELS:
            errors.append(f"Invalid effort: {effort}")

        return AgentValidationResult(valid=len(errors) == 0, errors=errors, warnings=warnings)
