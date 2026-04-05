"""Skills API router — DB-backed CRUD with keybinding support."""

from __future__ import annotations

import logging
from typing import Any, Callable, TYPE_CHECKING

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel, Field

if TYPE_CHECKING:
    from ephemeralos.skills.db.store import SkillDefinitionStore

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Request / response schemas
# ---------------------------------------------------------------------------


class SkillCreate(BaseModel):
    name: str = Field(min_length=1, max_length=128)
    description: str = Field(min_length=1)
    content: str = Field(min_length=1)
    keybinding: str | None = None


class SkillUpdate(BaseModel):
    description: str | None = None
    content: str | None = None
    keybinding: str | None = None


# ---------------------------------------------------------------------------
# Router factory
# ---------------------------------------------------------------------------


def create_skills_router(
    get_skill_store: Callable[[], "SkillDefinitionStore | None"],
) -> APIRouter:
    router = APIRouter(prefix="/api/skills", tags=["skills"])

    def _require_store() -> "SkillDefinitionStore":
        store = get_skill_store()
        if store is None:
            raise HTTPException(status_code=503, detail="Skill store not available (database not configured)")
        return store

    @router.get("")
    @router.get("/")
    async def list_skills() -> list[dict[str, Any]]:
        store = _require_store()
        records = store.list_active()
        return [
            {
                "name": r.name,
                "description": r.description,
                "source": r.source,
                "keybinding": r.keybinding,
            }
            for r in records
        ]

    @router.get("/{name}")
    async def get_skill(name: str) -> dict[str, Any]:
        store = _require_store()
        record = store.get_by_name(name)
        if record is None:
            raise HTTPException(status_code=404, detail=f"Skill '{name}' not found")
        return {
            "name": record.name,
            "description": record.description,
            "content": record.content,
            "source": record.source,
            "keybinding": record.keybinding,
            "version": record.version,
            "created_at": record.created_at.isoformat() if record.created_at else None,
            "updated_at": record.updated_at.isoformat() if record.updated_at else None,
        }

    @router.post("/", status_code=201)
    async def create_skill(body: SkillCreate) -> dict[str, Any]:
        store = _require_store()
        from uuid import uuid4
        from ephemeralos.skills.db.model import SkillDefinitionRecord

        if store.get_by_name(body.name) is not None:
            raise HTTPException(status_code=400, detail=f"Skill '{body.name}' already exists")

        record = SkillDefinitionRecord(
            id=str(uuid4()),
            name=body.name,
            description=body.description,
            content=body.content,
            source="user",
            keybinding=body.keybinding,
        )
        record = store.create(record)
        return {"name": record.name, "message": f"Skill '{record.name}' created"}

    @router.put("/{name}")
    async def update_skill(name: str, body: SkillUpdate) -> dict[str, Any]:
        store = _require_store()
        updates = body.model_dump(exclude_unset=True)
        if not updates:
            raise HTTPException(status_code=400, detail="No fields to update")
        try:
            record = store.update(name, updates)
        except KeyError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        return {
            "name": record.name,
            "description": record.description,
            "keybinding": record.keybinding,
            "version": record.version,
        }

    @router.delete("/{name}")
    async def delete_skill(name: str) -> dict[str, str]:
        store = _require_store()
        ok = store.soft_delete(name)
        if not ok:
            raise HTTPException(status_code=404, detail=f"Skill '{name}' not found")
        return {"deleted": name}

    return router
