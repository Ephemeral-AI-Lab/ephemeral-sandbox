"""Skills toolkit — lazy-loaded skill access via meta-tools.

Instead of injecting full skill content into the system prompt (which
can consume 10-50K+ tokens), this toolkit provides tools that let the
agent load skill content on demand.  The system prompt only contains
skill name + one-line description (~20 tokens each).

Follows Agno's progressive discovery pattern:
1. Agent sees skill summaries in system prompt
2. Agent calls ``load_skill`` to get full instructions when needed
3. Agent calls ``load_skill_reference`` for supplementary docs
4. Only relevant content consumes context tokens

Usage::

    from tools.builtins.skills import make_skills_toolkit

    toolkit = make_skills_toolkit(skill_registry, allowed_slugs=["skill-a", "skill-b"])
    tool_registry.register_toolkit(toolkit)
"""

from __future__ import annotations

import json
from typing import Any

from config.defaults import SKILL_REFERENCE_TRACE_LIMIT
from tools.core.base import BaseToolkit, ToolExecutionContext, ToolResult
from tools.core.decorator import tool
from skills.core.registry import SkillRegistry

_LOADED_SKILL_REFERENCES_KEY = "_loaded_skill_references_by_skill_this_turn"
_REQUIRED_NEXT_TOOL_KEY = "_required_next_tool"
_REFERENCE_TERMINAL_ACTIONS: dict[tuple[str, str], dict[str, str]] = {}


def get_reference_terminal_action(
    tool_name: str,
    tool_input: dict[str, object] | None,
) -> dict[str, str] | None:
    """Return terminal-action metadata for a terminal skill reference load."""
    if tool_name != "load_skill_reference" or not isinstance(tool_input, dict):
        return None
    skill_name = str(tool_input.get("skill_name") or "").strip()
    reference_name = str(tool_input.get("reference_name") or "").strip()
    if not skill_name or not reference_name:
        return None
    action = _REFERENCE_TERMINAL_ACTIONS.get((skill_name, reference_name))
    if action is None:
        return None
    out = {
        "tool_name": action["tool_name"],
        "skill_name": skill_name,
        "reference_name": reference_name,
        "reason": action["reason"],
    }
    reset_hint = str(action.get("reset_hint") or "").strip()
    if reset_hint:
        out["reset_hint"] = reset_hint
    return out


def get_required_next_tool(metadata: Any) -> dict[str, str] | None:
    """Return the active next-tool guard stored in runtime metadata."""
    if metadata is None:
        return None
    raw = metadata.get(_REQUIRED_NEXT_TOOL_KEY)
    if not isinstance(raw, dict):
        return None
    tool_name = str(raw.get("tool_name") or "").strip()
    if not tool_name:
        return None
    out = {"tool_name": tool_name}
    for key in ("skill_name", "reference_name", "reason", "reset_hint"):
        value = str(raw.get(key) or "").strip()
        if value:
            out[key] = value
    return out


def clear_required_next_tool(metadata: Any) -> None:
    """Clear any active next-tool guard from runtime metadata."""
    if metadata is None:
        return
    extras = getattr(metadata, "extras", None)
    if isinstance(extras, dict):
        extras.pop(_REQUIRED_NEXT_TOOL_KEY, None)
        return
    if isinstance(metadata, dict):
        metadata.pop(_REQUIRED_NEXT_TOOL_KEY, None)


def set_required_next_tool(
    context: ToolExecutionContext,
    *,
    tool_name: str,
    skill_name: str,
    reference_name: str,
    reason: str,
    reset_hint: str | None = None,
) -> None:
    """Arm a next-tool guard after a terminal skill reference loads."""
    payload = {
        "tool_name": tool_name,
        "skill_name": skill_name,
        "reference_name": reference_name,
        "reason": reason,
    }
    if reset_hint:
        payload["reset_hint"] = reset_hint
    context.metadata[_REQUIRED_NEXT_TOOL_KEY] = payload


def make_skills_toolkit(
    skill_registry: SkillRegistry,
    allowed_slugs: list[str] | None = None,
) -> BaseToolkit:
    """Create a skills toolkit scoped to the given skill slugs.

    If *allowed_slugs* is None, all registered skills are available.

    The toolkit provides two tools:

    - ``load_skill`` — load the full instructions (SKILL.md) of a skill
    - ``load_skill_reference`` — load a specific reference document from a skill
    """

    # Pre-resolve allowed skills for fast lookup
    available: dict[str, dict[str, object]] = {}
    slugs = (
        allowed_slugs
        if allowed_slugs is not None
        else [s.name for s in skill_registry.list_skills()]
    )
    for slug in slugs:
        skill = skill_registry.get(slug)
        if skill:
            available[skill.name] = {
                "name": skill.name,
                "description": skill.description,
                "references": list(skill.references.keys()),
            }

    def _loaded_skill_references(
        context: ToolExecutionContext,
        *,
        skill_name: str,
    ) -> list[str]:
        raw = context.metadata.get(_LOADED_SKILL_REFERENCES_KEY, {})
        if not isinstance(raw, dict):
            return []
        refs = raw.get(skill_name, [])
        if not isinstance(refs, list):
            return []
        out: list[str] = []
        for item in refs:
            if isinstance(item, str):
                stripped = item.strip()
                if stripped:
                    out.append(stripped)
        return out

    def _record_loaded_skill_reference(
        context: ToolExecutionContext,
        *,
        skill_name: str,
        reference_name: str,
    ) -> None:
        raw = context.metadata.get(_LOADED_SKILL_REFERENCES_KEY, {})
        loaded = raw.copy() if isinstance(raw, dict) else {}
        refs = _loaded_skill_references(context, skill_name=skill_name)
        refs.append(reference_name)
        if len(refs) > SKILL_REFERENCE_TRACE_LIMIT:
            refs = refs[-SKILL_REFERENCE_TRACE_LIMIT:]
        loaded[skill_name] = refs
        context.metadata[_LOADED_SKILL_REFERENCES_KEY] = loaded

    @tool(
        name="load_skill",
        description="Load the full instructions for a skill. Call this when a task matches a skill's description.",
        short_description="Load a skill's instructions.",
        read_only=True,
    )
    async def load_skill(
        skill_name: str,
        *,
        context: ToolExecutionContext,
    ) -> ToolResult:
        """Load full skill instructions by name.

        Args:
            skill_name: Name of the skill to load
        """
        if skill_name not in available:
            return ToolResult(
                output=json.dumps(
                    {
                        "error": f"Skill '{skill_name}' not found.",
                        "available": list(available.keys()),
                    }
                ),
                is_error=True,
            )

        skill = skill_registry.get(skill_name)
        if skill is None:
            return ToolResult(
                output=f"Skill '{skill_name}' not found in registry.",
                is_error=True,
            )

        # Include reference list so agent knows what's available to load next
        ref_names = list(skill.references.keys())
        if ref_names:
            footer = (
                "\n\n---\n"
                f"This skill has {len(ref_names)} reference document(s) available: "
                + ", ".join(f"`{r}`" for r in ref_names)
                + "\nUse `load_skill_reference` to load any of them."
            )
            return ToolResult(output=skill.content + footer)

        return ToolResult(output=skill.content)

    @tool(
        name="load_skill_reference",
        description="Load a reference document from a skill. References provide supplementary guidance, schemas, rubrics, or examples.",
        short_description="Load a skill reference.",
        read_only=True,
    )
    async def load_skill_reference(
        skill_name: str,
        reference_name: str,
        *,
        context: ToolExecutionContext,
    ) -> ToolResult:
        """Load a specific reference document from a skill.

        Args:
            skill_name: Name of the skill that owns the reference
            reference_name: Name of the reference document to load
        """
        if skill_name not in available:
            return ToolResult(
                output=json.dumps(
                    {
                        "error": f"Skill '{skill_name}' not found.",
                        "available": list(available.keys()),
                    }
                ),
                is_error=True,
            )

        skill = skill_registry.get(skill_name)
        if skill is None:
            return ToolResult(
                output=f"Skill '{skill_name}' not found in registry.",
                is_error=True,
            )

        content = skill.references.get(reference_name)
        if content is None:
            return ToolResult(
                output=json.dumps(
                    {
                        "error": f"Reference '{reference_name}' not found in skill '{skill_name}'.",
                        "available_references": list(skill.references.keys()),
                    }
                ),
                is_error=True,
            )

        _record_loaded_skill_reference(
            context,
            skill_name=skill_name,
            reference_name=reference_name,
        )
        terminal_action = get_reference_terminal_action(
            "load_skill_reference",
            {"skill_name": skill_name, "reference_name": reference_name},
        )
        if terminal_action is not None:
            set_required_next_tool(
                context,
                tool_name=terminal_action["tool_name"],
                skill_name=skill_name,
                reference_name=reference_name,
                reason=terminal_action["reason"],
                reset_hint=terminal_action.get("reset_hint"),
            )
        return ToolResult(output=content)

    # Build skill catalog for instructions
    skill_entries = []
    for info in available.values():
        refs = info.get("references", [])
        ref_note = f" ({len(refs)} references)" if refs else ""
        skill_entries.append(f'- `load_skill("{info["name"]}")` — {info["description"]}{ref_note}')

    instructions = (
        (
            "Lazy-loaded skill system. Use `load_skill(skill_name)` when a task "
            "matches a skill's domain. Use `load_skill_reference(skill_name, ref)` "
            "for supplementary docs.\n\n"
            "**Available skills:**\n" + "\n".join(skill_entries)
        )
        if skill_entries
        else None
    )

    toolkit = BaseToolkit(
        name="skills",
        description="Lazy-loaded skill instructions and reference documents",
        tools=[load_skill, load_skill_reference],
        instructions=instructions,
    )
    toolkit.available_skills = [
        {
            "name": str(info["name"]),
            "description": str(info["description"] or ""),
        }
        for info in sorted(available.values(), key=lambda item: str(item["name"]))
    ]
    return toolkit
