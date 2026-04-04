"""Model CRUD API routes."""

from __future__ import annotations

from typing import TYPE_CHECKING

from fastapi import APIRouter
from fastapi.responses import JSONResponse

from ephemeralos.models.api.schemas import RegisterModelRequest, SelectModelRequest

if TYPE_CHECKING:
    from ephemeralos.db.stores.model_store import ModelStore


def create_models_router(model_store: "ModelStore") -> APIRouter:
    """Build the model management API router."""
    router = APIRouter(prefix="/api/db/models", tags=["models"])

    def _db_available() -> bool:
        return model_store.is_available

    @router.get("")
    async def list_models():
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        models = model_store.list_all(redact=True)
        active = model_store.get_active(redact=True)
        return JSONResponse(content={
            "models": models,
            "active": active["key"] if active else None,
        })

    @router.get("/active")
    async def get_active_model():
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        active = model_store.get_active(redact=True)
        if active is None:
            return JSONResponse(status_code=404, content={"error": "No active model"})
        return JSONResponse(content=active)

    @router.get("/{key}")
    async def get_model(key: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        model = model_store.get(key, redact=True)
        if model is None:
            return JSONResponse(status_code=404, content={"error": "Model not found"})
        return JSONResponse(content=model)

    @router.post("/register")
    async def register_model(req: RegisterModelRequest):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        result = model_store.register(
            key=req.key,
            label=req.label,
            class_path=req.class_path,
            kwargs=req.kwargs,
            activate=req.activate,
        )
        return JSONResponse(content={"ok": True, "model": result})

    @router.post("/select")
    async def select_model(req: SelectModelRequest):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        result = model_store.select_active(req.key)
        if result is None:
            return JSONResponse(status_code=404, content={"error": "Model not found"})
        return JSONResponse(content={"ok": True, "model": result})

    @router.delete("/{key}")
    async def delete_model(key: str):
        if not _db_available():
            return JSONResponse(
                status_code=503,
                content={"error": "Database not configured"},
            )
        deleted = model_store.delete(key)
        if not deleted:
            return JSONResponse(status_code=404, content={"error": "Model not found"})
        return JSONResponse(content={"ok": True, "deleted": key})

    return router
