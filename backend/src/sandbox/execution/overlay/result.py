"""Snapshot overlay result values and result-file helpers."""

from __future__ import annotations

import json
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path
from types import MappingProxyType
from typing import Any

from sandbox.layer_stack.manifest import Manifest
from sandbox.execution.overlay.change import OverlayPathChange


@dataclass(frozen=True)
class OverlayCapture:
    """Policy-blind shell execution result captured from a snapshot overlay."""

    exit_code: int
    stdout_ref: str
    stderr_ref: str
    snapshot_version: int
    changes: tuple[OverlayPathChange, ...]
    snapshot_manifest: Manifest | None = None
    timings: Mapping[str, float] = field(default_factory=dict)

    def __post_init__(self) -> None:
        object.__setattr__(self, "exit_code", int(self.exit_code))
        object.__setattr__(self, "snapshot_version", int(self.snapshot_version))
        object.__setattr__(self, "changes", tuple(self.changes))
        object.__setattr__(
            self,
            "timings",
            MappingProxyType(
                {str(key): float(value) for key, value in self.timings.items()}
            ),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "exit_code": self.exit_code,
            "stdout_ref": self.stdout_ref,
            "stderr_ref": self.stderr_ref,
            "snapshot_version": self.snapshot_version,
            "changes": [change.to_dict() for change in self.changes],
            "snapshot_manifest": (
                self.snapshot_manifest.to_dict()
                if self.snapshot_manifest is not None
                else None
            ),
            "timings": dict(self.timings),
        }

    @classmethod
    def from_dict(cls, payload: Mapping[str, Any]) -> OverlayCapture:
        raw_changes = payload.get("changes") or []
        if not isinstance(raw_changes, list):
            raise ValueError("OverlayCapture.changes must be a list")
        if not all(isinstance(change, Mapping) for change in raw_changes):
            raise ValueError("OverlayCapture.changes entries must be objects")
        return cls(
            exit_code=int(payload["exit_code"]),
            stdout_ref=str(payload["stdout_ref"]),
            stderr_ref=str(payload["stderr_ref"]),
            snapshot_version=int(payload["snapshot_version"]),
            changes=tuple(OverlayPathChange.from_dict(change) for change in raw_changes),
            snapshot_manifest=(
                Manifest.from_dict(payload["snapshot_manifest"])
                if payload.get("snapshot_manifest") is not None
                else None
            ),
            timings={
                str(key): float(value)
                for key, value in (payload.get("timings") or {}).items()
            },
        )


def write_overlay_capture(run_dir: str | Path, capture: OverlayCapture) -> str:
    path = Path(run_dir) / "result.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(capture.to_dict(), separators=(",", ":"), sort_keys=True),
        encoding="utf-8",
    )
    return str(path)


def read_output_ref(path: str) -> str:
    return Path(path).read_bytes().decode("utf-8", "replace")


__all__ = [
    "OverlayCapture",
    "read_output_ref",
    "write_overlay_capture",
]
