"""Task-input token parsing for executor-action scenarios.

The mock executor receives a `instruction` string built by the planner and
formatted by the mock runner. Scenarios read scalar fields from the string
via `key=value` tokens.
"""

from __future__ import annotations


def instruction_field(text: str, name: str) -> str | None:
    """Return the value of `<name>=<value>` token from a space-separated string."""
    prefix = f"{name}="
    for part in text.split():
        if part.startswith(prefix):
            return part[len(prefix) :].strip()
    return None


__all__ = ["instruction_field"]
