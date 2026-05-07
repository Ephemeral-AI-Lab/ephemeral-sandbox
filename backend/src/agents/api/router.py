"""Agent definition API router for config-backed definitions."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any
from collections.abc import Callable

from fastapi import APIRouter, HTTPException

from agents.registry import get_definition, list_definitions
from agents.api.schemas import AgentValidationRequest
from agents.validation import AgentDefinitionValidator, AgentValidationResult
from tools import collect_tool_catalog

if TYPE_CHECKING:
    from tools import ToolRegistry

_READ_ONLY_DETAIL = "Agent definitions are file-backed under backend/config/agents."


def create_agents_router(
    get_tool_registry: Callable[[], ToolRegistry | None],
) -> APIRouter:
    router = APIRouter(prefix="/api/agents", tags=["agents"])

    @router.get("")
    @router.get("/")
    async def list_agents() -> list[dict[str, Any]]:
        defs = list_definitions()
        return [
            {
                "name": d.name,
                "description": d.description,
                "model": d.model,
                "background": d.background,
            }
            for d in defs
        ]

    @router.get("/tools/available")
    async def list_available_tools() -> list[dict[str, str]]:
        tr = get_tool_registry()
        return [
            {"name": entry.name, "description": entry.description}
            for entry in collect_tool_catalog(tr, include_runtime_tools=True)
        ]

    @router.get("/{name}")
    async def get_agent(name: str) -> dict[str, Any]:
        defn = get_definition(name)
        if defn is None:
            raise HTTPException(status_code=404, detail=f"Agent '{name}' not found")
        return defn.model_dump()

    @router.post("/", status_code=201)
    async def create_agent() -> dict[str, str]:
        raise HTTPException(status_code=405, detail=_READ_ONLY_DETAIL)

    @router.put("/{name}")
    async def update_agent(name: str) -> dict[str, str]:
        raise HTTPException(status_code=405, detail=_READ_ONLY_DETAIL)

    @router.delete("/{name}")
    async def delete_agent(name: str) -> dict[str, str]:
        raise HTTPException(status_code=405, detail=_READ_ONLY_DETAIL)

    @router.post("/{name}/clone", status_code=201)
    async def clone_agent(name: str) -> dict[str, str]:
        raise HTTPException(status_code=405, detail=_READ_ONLY_DETAIL)

    @router.post("/validate", response_model=AgentValidationResult)
    async def validate_agent(body: AgentValidationRequest) -> AgentValidationResult:
        return AgentDefinitionValidator(get_tool_registry()).validate(body)

    return router
