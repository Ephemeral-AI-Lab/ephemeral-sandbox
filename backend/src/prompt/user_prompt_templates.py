"""Markdown-backed user prompt templates for team runtime agents."""

from __future__ import annotations

import re
from functools import lru_cache
from pathlib import Path
from typing import Mapping


_PROMPT_DIR = Path(__file__).resolve().parent / "user_prompt"
_CONDITIONAL_RE = re.compile(
    r"{{#if\s+(?P<name>[a-zA-Z_][a-zA-Z0-9_]*)\s*}}(?P<body>.*?){{/if}}",
    re.DOTALL,
)
_PLACEHOLDER_RE = re.compile(r"{{\s*(?P<name>[a-zA-Z_][a-zA-Z0-9_]*)\s*}}")
_QUAD_FENCE_RE = re.compile(r"````(?:text)?\s*\n(?P<body>.*?)\n````", re.DOTALL)


class UserPromptTemplateError(RuntimeError):
    """Raised when a markdown user prompt template cannot be loaded."""


def _prompt_path(name: str) -> Path:
    if "/" in name or "\\" in name or name.startswith("."):
        raise UserPromptTemplateError(f"invalid user prompt template name: {name!r}")
    return _PROMPT_DIR / f"{name}.md"


@lru_cache(maxsize=None)
def _read_prompt_file(name: str) -> str:
    path = _prompt_path(name)
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        raise UserPromptTemplateError(f"failed to read user prompt template {path}") from exc


@lru_cache(maxsize=None)
def load_user_prompt_template(name: str) -> str:
    """Load a source template from ``user_prompt/<name>.md``."""
    content = _read_prompt_file(name)
    match = _QUAD_FENCE_RE.search(content)
    if match is not None:
        return match.group("body").strip()
    return content.strip()


def _is_truthy(value: object) -> bool:
    if value is None:
        return False
    if isinstance(value, str):
        return bool(value.strip())
    if isinstance(value, (list, tuple, set, dict)):
        return bool(value)
    return bool(value)


def _stringify(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value.strip()
    if isinstance(value, (list, tuple, set)):
        return "\n".join(str(item).strip() for item in value if str(item).strip())
    return str(value).strip()


def render_user_prompt_template(name: str, variables: Mapping[str, object]) -> str:
    """Render a markdown user prompt template with simple variables and conditionals."""
    template = load_user_prompt_template(name)

    def replace_conditional(match: re.Match[str]) -> str:
        key = match.group("name")
        if not _is_truthy(variables.get(key)):
            return ""
        return match.group("body")

    rendered = _CONDITIONAL_RE.sub(replace_conditional, template)

    def replace_placeholder(match: re.Match[str]) -> str:
        return _stringify(variables.get(match.group("name")))

    rendered = _PLACEHOLDER_RE.sub(replace_placeholder, rendered)
    return re.sub(r"\n{3,}", "\n\n", rendered).strip()
