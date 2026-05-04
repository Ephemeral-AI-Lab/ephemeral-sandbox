"""Runtime result envelope for policy-blind overlay shell execution."""

from __future__ import annotations

import json
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from sandbox.overlay.capture.changes import UpperChange


@dataclass(frozen=True)
class RuntimeResultEnvelope:
    exit_code: int
    stdout_ref: str
    stderr_ref: str
    snapshot_version: int
    upper_changes: tuple[UpperChange, ...]

    def __post_init__(self) -> None:
        object.__setattr__(self, "exit_code", int(self.exit_code))
        object.__setattr__(self, "snapshot_version", int(self.snapshot_version))
        object.__setattr__(self, "upper_changes", tuple(self.upper_changes))

    def to_dict(self) -> dict[str, Any]:
        return {
            "exit_code": self.exit_code,
            "stdout_ref": self.stdout_ref,
            "stderr_ref": self.stderr_ref,
            "snapshot_version": self.snapshot_version,
            "upper_changes": [change.to_dict() for change in self.upper_changes],
        }

    @classmethod
    def from_dict(cls, payload: Mapping[str, Any]) -> "RuntimeResultEnvelope":
        raw_changes = payload.get("upper_changes") or []
        if not isinstance(raw_changes, list):
            raise ValueError("RuntimeResultEnvelope.upper_changes must be a list")
        if not all(isinstance(change, Mapping) for change in raw_changes):
            raise ValueError("RuntimeResultEnvelope.upper_changes entries must be objects")
        return cls(
            exit_code=int(payload["exit_code"]),
            stdout_ref=str(payload["stdout_ref"]),
            stderr_ref=str(payload["stderr_ref"]),
            snapshot_version=int(payload["snapshot_version"]),
            upper_changes=tuple(
                UpperChange.from_dict(change) for change in raw_changes
            ),
        )


def write_result_envelope(
    run_dir: str | Path,
    envelope: RuntimeResultEnvelope,
) -> str:
    path = Path(run_dir) / "result.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(envelope.to_dict(), separators=(",", ":"), sort_keys=True),
        encoding="utf-8",
    )
    return str(path)


__all__ = [
    "RuntimeResultEnvelope",
    "write_result_envelope",
]
