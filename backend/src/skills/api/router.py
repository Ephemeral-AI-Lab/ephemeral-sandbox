"""Skills API router for config-backed skill definitions."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from fastapi import APIRouter, HTTPException
from fastapi.responses import PlainTextResponse
from pydantic import BaseModel, Field

from config.paths import get_builtin_skills_dir
from skills.bundled import get_bundled_skills

# Packaged skills directory — read-only skill content shipped with the codebase
_PACKAGED_SKILLS_DIR = get_builtin_skills_dir()


def _resolve_packaged_skill_dir(name: str) -> Path | None:
    """Find the on-disk directory for a packaged skill by name."""
    candidate = _PACKAGED_SKILLS_DIR / name
    if candidate.is_dir():
        return candidate
    return None


def _build_file_tree(root: Path, base: Path | None = None) -> list[dict[str, Any]]:
    """Recursively build a file tree listing."""
    if base is None:
        base = root
    entries: list[dict[str, Any]] = []
    for item in sorted(root.iterdir()):
        if item.name.startswith(".") or item.name == "__pycache__":
            continue
        rel = str(item.relative_to(base))
        if item.is_dir():
            entries.append({
                "name": item.name,
                "type": "directory",
                "path": rel,
                "children": _build_file_tree(item, base),
            })
        else:
            entries.append({
                "name": item.name,
                "type": "file",
                "path": rel,
                "size": item.stat().st_size,
            })
    return entries


# ---------------------------------------------------------------------------
# Request / response schemas
# ---------------------------------------------------------------------------


class SkillCreate(BaseModel):
    name: str = Field(min_length=1, max_length=128)
    description: str = Field(min_length=1)
    content: str = Field(min_length=1)


class SkillUpdate(BaseModel):
    description: str | None = None
    content: str | None = None


# ---------------------------------------------------------------------------
# Router factory
# ---------------------------------------------------------------------------


_READ_ONLY_DETAIL = "Skill definitions are file-backed under backend/config/skills."


def create_skills_router() -> APIRouter:
    router = APIRouter(prefix="/api/skills", tags=["skills"])

    @router.get("")
    @router.get("/")
    async def list_skills() -> list[dict[str, Any]]:
        return [
            {
                "name": skill.name,
                "description": skill.description,
            }
            for skill in get_bundled_skills()
        ]

    @router.get("/{name}")
    async def get_skill(name: str) -> dict[str, Any]:
        skill = next((item for item in get_bundled_skills() if item.name == name), None)
        if skill is None:
            raise HTTPException(status_code=404, detail=f"Skill '{name}' not found")
        return {
            "name": skill.name,
            "description": skill.description,
            "content": skill.content,
            "source": skill.source,
            "path": skill.path,
        }

    @router.post("/", status_code=201)
    async def create_skill(body: SkillCreate) -> dict[str, str]:
        raise HTTPException(status_code=405, detail=_READ_ONLY_DETAIL)

    @router.put("/{name}")
    async def update_skill(name: str, body: SkillUpdate) -> dict[str, str]:
        raise HTTPException(status_code=405, detail=_READ_ONLY_DETAIL)

    @router.delete("/{name}")
    async def delete_skill(name: str) -> dict[str, str]:
        raise HTTPException(status_code=405, detail=_READ_ONLY_DETAIL)

    @router.get("/{name}/files")
    async def list_packaged_skill_files(name: str) -> dict[str, Any]:
        """Return the file tree for a packaged skill's on-disk directory."""
        skill_dir = _resolve_packaged_skill_dir(name)
        if skill_dir is None:
            return {"name": name, "tree": []}
        return {"name": name, "tree": _build_file_tree(skill_dir)}

    @router.get("/{name}/files/{file_path:path}")
    async def get_packaged_skill_file(name: str, file_path: str) -> PlainTextResponse:
        """Serve a specific file from a packaged skill's directory."""
        skill_dir = _resolve_packaged_skill_dir(name)
        if skill_dir is None:
            raise HTTPException(status_code=404, detail=f"Packaged skill directory for '{name}' not found")

        target = (skill_dir / file_path).resolve()
        # Prevent path traversal
        try:
            target.relative_to(skill_dir)
        except ValueError:
            raise HTTPException(status_code=403, detail="Path traversal not allowed")

        if not target.is_file():
            raise HTTPException(status_code=404, detail=f"File '{file_path}' not found")

        try:
            content = target.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            raise HTTPException(status_code=415, detail="Binary files not supported")

        return PlainTextResponse(content)

    return router
