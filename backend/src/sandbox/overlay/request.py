"""Request values for one leased snapshot overlay shell call."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class OverlayShellRequest:
    """One per-call shell request against a leased layer-stack snapshot."""

    request_id: str
    command: tuple[str, ...]
    cwd: str
    env: Mapping[str, str]
    timeout_seconds: float | None

    def __post_init__(self) -> None:
        request_id = str(self.request_id).strip()
        if not request_id:
            raise ValueError("request_id must not be empty")
        command = tuple(str(part) for part in self.command)
        if not command or any(part == "" for part in command):
            raise ValueError("command must contain non-empty argv parts")
        timeout = self.timeout_seconds
        if timeout is not None and timeout <= 0:
            raise ValueError("timeout_seconds must be positive when provided")
        object.__setattr__(self, "request_id", request_id)
        object.__setattr__(self, "command", command)
        object.__setattr__(self, "cwd", str(self.cwd).strip() or ".")
        object.__setattr__(
            self,
            "env",
            {str(key): str(value) for key, value in self.env.items()},
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "request_id": self.request_id,
            "command": list(self.command),
            "cwd": self.cwd,
            "env": dict(self.env),
            "timeout_seconds": self.timeout_seconds,
        }

    @classmethod
    def from_dict(cls, payload: Mapping[str, Any]) -> OverlayShellRequest:
        command_raw = payload.get("command")
        if not isinstance(command_raw, list):
            raise ValueError("OverlayShellRequest.command must be a list")
        env_raw = payload.get("env") or {}
        if not isinstance(env_raw, Mapping):
            raise ValueError("OverlayShellRequest.env must be an object")
        timeout_raw = payload.get("timeout_seconds")
        return cls(
            request_id=str(payload.get("request_id") or ""),
            command=tuple(str(part) for part in command_raw),
            cwd=str(payload.get("cwd") or "."),
            env={str(key): str(value) for key, value in env_raw.items()},
            timeout_seconds=float(timeout_raw) if timeout_raw is not None else None,
        )


__all__ = ["OverlayShellRequest"]
