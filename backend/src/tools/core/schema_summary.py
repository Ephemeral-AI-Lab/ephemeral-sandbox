"""Human-readable tool schema summaries generated from live tool objects."""

from __future__ import annotations

import json
from pathlib import Path
from types import UnionType
from typing import Any, Literal, Sequence, get_args, get_origin

from pydantic import BaseModel, RootModel
from pydantic_core import PydanticUndefined

from skills.core.loader import load_skill_registry
from tools.core.base import BaseToolkit, BaseTool
from tools.core.factory import ToolkitContext, create_toolkit, list_toolkits


def collect_schema_toolkits(
    *,
    cwd: str | Path | None = None,
    sandbox_id: str = "schema-dump",
    caller_agent: str = "",
) -> list[BaseToolkit]:
    """Instantiate toolkits suitable for live schema inspection."""
    ctx = ToolkitContext(
        metadata={
            "sandbox_id": sandbox_id,
            "agent_name": caller_agent,
        }
    )
    toolkits = [create_toolkit(name, ctx) for name in sorted(list_toolkits())]

    from tools.builtins.background import make_background_toolkit
    from tools.builtins.skills import make_skills_toolkit

    background_tool_names = sorted(
        {
            tool.name
            for toolkit in toolkits
            for tool in toolkit.list_tools()
            if getattr(tool, "background", "forbidden") != "forbidden"
        }
    )
    if background_tool_names:
        toolkits.append(make_background_toolkit(background_tool_names))

    skills_toolkit = make_skills_toolkit(load_skill_registry(cwd))
    if skills_toolkit.list_tools():
        toolkits.append(skills_toolkit)

    return _dedupe_toolkits(toolkits)


def format_tool_schema_summary(
    toolkits: Sequence[BaseToolkit],
    *,
    include_descriptions: bool = True,
) -> str:
    """Render a human-readable input/output schema summary for each tool."""
    sections: list[str] = []
    for toolkit in toolkits:
        lines = [f"Toolkit: {toolkit.name}"]
        if include_descriptions and toolkit.description:
            lines.append(f"  {toolkit.description}")
        for tool in toolkit.list_tools():
            lines.extend(_format_tool(tool, include_descriptions=include_descriptions))
        sections.append("\n".join(lines))
    return "\n\n".join(sections)


def _dedupe_toolkits(toolkits: list[BaseToolkit]) -> list[BaseToolkit]:
    by_name: dict[str, BaseToolkit] = {}
    for toolkit in toolkits:
        current = by_name.get(toolkit.name)
        if current is None:
            by_name[toolkit.name] = toolkit
            continue
        existing_names = set(current.tool_names())
        for tool in toolkit.list_tools():
            if tool.name not in existing_names:
                current.register(tool)
                existing_names.add(tool.name)
    return sorted(by_name.values(), key=lambda toolkit: toolkit.name)


def _format_tool(tool: BaseTool, *, include_descriptions: bool) -> list[str]:
    lines = [f"  {tool.name}"]
    if include_descriptions and tool.description:
        lines.append(f"    description: {tool.description}")
    lines.extend(
        _format_model(
            "input",
            tool.input_model,
            include_descriptions=include_descriptions,
        )
    )
    lines.extend(
        _format_model(
            "output",
            tool.output_model,
            include_descriptions=include_descriptions,
        )
    )
    return lines


def _format_model(
    label: str,
    model: type[BaseModel],
    *,
    include_descriptions: bool,
) -> list[str]:
    if issubclass(model, RootModel):
        description = _clean_description(model.__doc__ or "")
        root = model.model_fields.get("root")
        root_type = _type_name(root.annotation) if root is not None else "any"
        line = f"    {label}: {root_type}"
        if include_descriptions and description:
            line += f" - {description}"
        return [line]
    if not model.model_fields:
        return [f"    {label}: no fields"]
    lines = [f"    {label}:"]
    lines.extend(
        _format_field(name, field_info, include_descriptions=include_descriptions)
        for name, field_info in model.model_fields.items()
    )
    return lines


def _format_field(name: str, field_info: Any, *, include_descriptions: bool) -> str:
    prefix = f"      - {name}: {_type_name(field_info.annotation)} [{_cardinality(field_info)}]"
    description = _clean_description(str(field_info.description or ""))
    if include_descriptions and description:
        return f"{prefix} - {description}"
    return prefix


def _type_name(tp: Any) -> str:
    if tp is Any:
        return "any"
    if tp is None or tp is type(None):
        return "null"
    origin = get_origin(tp)
    if origin is UnionType or str(origin) == "typing.Union":
        return " | ".join(_type_name(arg) for arg in get_args(tp))
    if origin is list:
        args = get_args(tp)
        return f"list[{_type_name(args[0])}]" if args else "list"
    if origin is dict:
        args = get_args(tp)
        return (
            f"dict[{_type_name(args[0])}, {_type_name(args[1])}]"
            if len(args) == 2
            else "dict"
        )
    if origin is Literal:
        return " | ".join(_literal_name(arg) for arg in get_args(tp))
    if isinstance(tp, type):
        return tp.__name__
    return str(tp).replace("typing.", "")


def _cardinality(field_info: Any) -> str:
    if field_info.is_required():
        return "required"
    default_factory = getattr(field_info, "default_factory", None)
    if default_factory is not None and default_factory is not PydanticUndefined:
        return _default_factory_label(field_info.annotation)
    value = field_info.default
    if value is PydanticUndefined:
        return _default_factory_label(field_info.annotation)
    if value is None:
        return "default null"
    if isinstance(value, str):
        return f"default {json.dumps(value)}"
    return f"default {value!r}"


def _default_factory_label(tp: Any) -> str:
    origins = _annotation_origins(tp)
    if list in origins:
        return "default []"
    if dict in origins:
        return "default {}"
    if set in origins:
        return "default set()"
    return "default factory"


def _annotation_origins(tp: Any) -> set[Any]:
    origin = get_origin(tp)
    if origin is UnionType or str(origin) == "typing.Union":
        origins: set[Any] = set()
        for arg in get_args(tp):
            origins.update(_annotation_origins(arg))
        return origins
    return {origin or tp}


def _literal_name(value: Any) -> str:
    if isinstance(value, str):
        return f'"{value}"'
    if value is None:
        return "null"
    return repr(value)


def _clean_description(value: str) -> str:
    return " ".join(str(value or "").strip().split())
