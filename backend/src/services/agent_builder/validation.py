"""Validation service for agent definitions."""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING

from ephemeralos.coordinator.agent_definitions import (
    AGENT_COLORS,
    EFFORT_LEVELS,
    MEMORY_SCOPES,
    PERMISSION_MODES,
)
from ephemeralos.toolkits.factory import has_factory
from ephemeralos.ui.schemas.agent_schemas import AgentValidationResult

if TYPE_CHECKING:
    from ephemeralos.tools.base import ToolRegistry
    from ephemeralos.ui.schemas.agent_schemas import AgentDefinitionCreate, AgentDefinitionUpdate

logger = logging.getLogger(__name__)


class AgentDefinitionValidator:
    """Validates that agent definition references (tools, toolkits, skills) are resolvable."""

    def __init__(self, tool_registry: ToolRegistry) -> None:
        self._tool_registry = tool_registry

    def validate(
        self, defn: AgentDefinitionCreate | AgentDefinitionUpdate
    ) -> AgentValidationResult:
        """Validate an agent definition. Returns errors and warnings."""
        errors: list[str] = []
        warnings: list[str] = []

        # Check tool names exist in ToolRegistry
        tools = getattr(defn, "tools", None)
        if tools:
            for t in tools:
                if t != "*" and self._tool_registry.get(t) is None:
                    warnings.append(f"Unknown tool: {t}")

        # Check toolkit names have registered factories
        toolkits = getattr(defn, "toolkits", None)
        if toolkits:
            for tk in toolkits:
                if not has_factory(tk):
                    errors.append(f"Unknown toolkit factory: {tk}")

        # Validate enum fields (belt-and-suspenders — Pydantic validators catch most)
        effort = getattr(defn, "effort", None)
        if effort is not None and effort not in EFFORT_LEVELS:
            errors.append(f"Invalid effort: {effort}")

        color = getattr(defn, "color", None)
        if color is not None and color not in AGENT_COLORS:
            errors.append(f"Invalid color: {color}")

        permission_mode = getattr(defn, "permission_mode", None)
        if permission_mode is not None and permission_mode not in PERMISSION_MODES:
            errors.append(f"Invalid permission_mode: {permission_mode}")

        memory = getattr(defn, "memory", None)
        if memory is not None and memory not in MEMORY_SCOPES:
            errors.append(f"Invalid memory scope: {memory}")

        return AgentValidationResult(
            valid=len(errors) == 0,
            errors=errors,
            warnings=warnings,
        )
