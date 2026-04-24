"""Agent definition validation exports."""

from __future__ import annotations

__all__ = ["AgentDefinitionValidator"]


def __getattr__(name: str) -> object:
    if name == "AgentDefinitionValidator":
        from agents.builder.validation import AgentDefinitionValidator

        return AgentDefinitionValidator
    raise AttributeError(name)
