"""Pydantic request/response models for the models API."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel


class RegisterModelRequest(BaseModel):
    key: str
    label: str
    class_path: str
    kwargs: dict[str, Any] = {}
    activate: bool = True


class SelectModelRequest(BaseModel):
    key: str
