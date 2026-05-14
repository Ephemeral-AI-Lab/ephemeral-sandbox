"""Task-input parsing helpers shared across executor-action scenarios.

The mock executor receives a `rendered_prompt` string built by the planner and
formatted by the squad runner. Scenarios read scalar fields from the string
via `key=value` tokens. This helper centralizes the parser used in the
existing composite scenarios so new scenarios don't reinvent it.
"""

from __future__ import annotations


def field(text: str, name: str) -> str | None:
    """Return the value of `<name>=<value>` token from a space-separated string."""
    prefix = f"{name}="
    for part in text.split():
        if part.startswith(prefix):
            return part[len(prefix) :].strip()
    return None


__all__ = ["field"]
