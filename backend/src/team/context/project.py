"""Tier 1 — project-level context for a TeamRun."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class ProjectContext:
    goal: str = ""
    user_request: str = ""
    rationale_history: list[str] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)

    def add_rationale(self, text: str) -> None:
        if text:
            self.rationale_history.append(text)

    def add_note(self, text: str) -> None:
        if text:
            self.notes.append(text)

    def to_dict(self) -> dict[str, Any]:
        return {
            "goal": self.goal,
            "user_request": self.user_request,
            "rationale_history": list(self.rationale_history),
            "notes": list(self.notes),
        }
