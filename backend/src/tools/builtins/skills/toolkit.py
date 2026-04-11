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

from tools.core.base import BaseToolkit, ToolExecutionContext, ToolResult
from tools.core.decorator import tool
from skills.core.registry import SkillRegistry


def make_skills_toolkit(
    skill_registry: SkillRegistry,
    allowed_slugs: list[str] | None = None,
) -> BaseToolkit:
    """Create a skills toolkit scoped to the given skill slugs.

    If *allowed_slugs* is None, all registered skills are available.

    The toolkit provides three tools:

    - ``list_skills`` — list available skills with descriptions and reference names
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

    def _is_benchmark_root_planner(context: ToolExecutionContext) -> bool:
        if str(context.metadata.get("agent_name") or "").strip() != "team_planner":
            return False
        team_run_id = str(context.metadata.get("team_run_id") or "").strip()
        work_item_id = str(context.metadata.get("work_item_id") or "").strip()
        if not team_run_id or not work_item_id:
            return False
        try:
            from team.runtime.registry import get as get_team_run
        except Exception:
            return False
        try:
            team_run = get_team_run(team_run_id)
        except Exception:
            return False
        if team_run is None or work_item_id != str(getattr(team_run, "root_work_item_id", "") or ""):
            return False
        graph = getattr(getattr(team_run, "dispatcher", None), "graph", None)
        if not isinstance(graph, dict):
            return False
        root_item = graph.get(work_item_id)
        payload = getattr(root_item, "payload", None)
        if not isinstance(payload, dict):
            return False
        return bool(payload.get("fail_to_pass") or payload.get("pass_to_pass"))

    def _has_scout_wave(context: ToolExecutionContext) -> bool:
        raw = context.metadata.get("_scout_target_paths_this_turn", [])
        if isinstance(raw, str):
            return bool(raw.strip())
        if isinstance(raw, list):
            return any(isinstance(item, str) and item.strip() for item in raw)
        return False

    @tool(
        name="load_skill",
        description="Load the full instructions for a skill. Call this when a task matches a skill's description.",
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

        if (
            skill_name == "team-planner-playbook"
            and reference_name in {"plan-json-contract", "task-planning-decomposition"}
            and _is_benchmark_root_planner(context)
            and not _has_scout_wave(context)
        ):
            return ToolResult(
                output=(
                    "Fresh benchmark-root planners must not load final-plan references "
                    f"like `{reference_name}` before the first scout wave. "
                    "Launch at least one bounded scout now with "
                    "`run_subagent(agent_name=\"scout\", input={\"target_paths\": [...]}, task_note=\"...\")` "
                    "on the next unresolved production-owner slice, wait for the scout brief, "
                    "then load the final-plan reference when you are ready to draft the DAG."
                ),
                is_error=True,
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

    return BaseToolkit(
        name="skills",
        description="Lazy-loaded skill instructions and reference documents",
        tools=[load_skill, load_skill_reference],
        instructions=instructions,
    )
