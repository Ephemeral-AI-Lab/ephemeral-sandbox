"""Dataclasses and exceptions for overlay upperdir capture."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import Any, Literal


UpperChangeKind = Literal["regular", "whiteout", "symlink", "opaque_dir"]


class OverlayError(RuntimeError):
    """Base error for overlay auditing failures."""


class OverlayRunError(OverlayError):
    """Raised when the sandbox-side overlay runtime transport fails."""


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


@dataclass(frozen=True)
class OverlayLease:
    """One per-op overlay lease.

    The overlay model (see plan §0, "Mount model") has no pool — each
    ``svc.cmd`` builds a fresh unshare namespace with fresh mounts and
    tears it all down on exit. The lease is just the per-op run
    directory on the container filesystem (outside the overlay so it
    survives ns exit) that holds ``diff.ndjson``.
    """

    run_dir: str


@dataclass(frozen=True)
class UpperChange:
    """One raw upperdir change emitted by the overlay runtime for OCC."""

    rel: str
    kind: UpperChangeKind
    base_bytes: bytes | None
    upper_bytes: bytes | None
    base_existed: bool


@dataclass(frozen=True)
class OverlayCapture:
    """Parsed ``diff.ndjson`` payload after one overlay op."""

    exit_code: int
    upper_bytes: int
    upper_files: int
    upper_changes: tuple[UpperChange, ...]
    run_timings: dict[str, float] = field(default_factory=dict)
    warnings: tuple[str, ...] = ()


@dataclass
class OverlayRunOutcome:
    """Capture-run handoff between overlay and its caller.

    Overlay produces raw :attr:`upper_changes`; the caller drives merge
    policy. Not ``frozen`` because ``overlay_stage_timings`` is set after
    lease cleanup.
    """

    exit_code: int
    stdout: str
    upper_changes: tuple[UpperChange, ...]
    warnings: tuple[str, ...] = ()
    overlay_run_timings: dict[str, float] = field(default_factory=dict)
    overlay_stage_timings: dict[str, float] = field(default_factory=dict)


def overlay_shell_request_to_dict(request: OverlayShellRequest) -> dict[str, Any]:
    return {
        "request_id": request.request_id,
        "command": list(request.command),
        "cwd": request.cwd,
        "env": dict(request.env),
        "timeout_seconds": request.timeout_seconds,
    }


def overlay_shell_request_from_dict(payload: Mapping[str, Any]) -> OverlayShellRequest:
    command_raw = payload.get("command")
    if not isinstance(command_raw, list):
        raise ValueError("OverlayShellRequest.command must be a list")
    env_raw = payload.get("env") or {}
    if not isinstance(env_raw, Mapping):
        raise ValueError("OverlayShellRequest.env must be an object")
    timeout_raw = payload.get("timeout_seconds")
    return OverlayShellRequest(
        request_id=str(payload.get("request_id") or ""),
        command=tuple(str(part) for part in command_raw),
        cwd=str(payload.get("cwd") or "."),
        env={str(key): str(value) for key, value in env_raw.items()},
        timeout_seconds=float(timeout_raw) if timeout_raw is not None else None,
    )


__all__ = [
    "OverlayCapture",
    "OverlayError",
    "OverlayLease",
    "OverlayRunError",
    "OverlayRunOutcome",
    "OverlayShellRequest",
    "UpperChange",
    "UpperChangeKind",
    "overlay_shell_request_from_dict",
    "overlay_shell_request_to_dict",
]
