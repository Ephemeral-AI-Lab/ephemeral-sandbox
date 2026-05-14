"""Policy-blind path changes captured from a snapshot overlay."""

from __future__ import annotations

import hashlib
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal

from sandbox.layer_stack.layer_change import normalize_layer_path

OverlayPathChangeKind = Literal["write", "delete", "symlink", "opaque_dir"]


@dataclass(frozen=True)
class OverlayPathChange:
    path: str
    kind: OverlayPathChangeKind
    content_path: str | None
    final_hash: str | None

    def __post_init__(self) -> None:
        object.__setattr__(
            self,
            "path",
            normalize_layer_path(self.path, allow_root=self.kind == "opaque_dir"),
        )
        if self.kind in ("write", "symlink"):
            if not self.content_path:
                raise ValueError(f"{self.kind} changes require content_path")
            if not self.final_hash:
                raise ValueError(f"{self.kind} changes require final_hash")
            return
        if self.content_path is not None:
            raise ValueError(f"{self.kind} changes must not carry content_path")
        if self.final_hash is not None:
            raise ValueError(f"{self.kind} changes must not carry final_hash")

    def to_dict(self) -> dict[str, str | None]:
        return {
            "path": self.path,
            "kind": self.kind,
            "content_path": self.content_path,
            "final_hash": self.final_hash,
        }

    @classmethod
    def from_dict(cls, payload: Mapping[str, Any]) -> OverlayPathChange:
        kind = payload["kind"]
        if kind not in ("write", "delete", "symlink", "opaque_dir"):
            raise ValueError(f"unsupported upper change kind: {kind!r}")
        return cls(
            path=str(payload["path"]),
            kind=kind,
            content_path=(
                str(payload["content_path"])
                if payload.get("content_path") is not None
                else None
            ),
            final_hash=(
                str(payload["final_hash"])
                if payload.get("final_hash") is not None
                else None
            ),
        )


def content_hash(path: str | Path, *, symlink: bool = False) -> str:
    data = (
        Path(path).readlink().as_posix().encode("utf-8")
        if symlink
        else Path(path).read_bytes()
    )
    return hashlib.sha256(data).hexdigest()


__all__ = [
    "OverlayPathChange",
    "OverlayPathChangeKind",
    "content_hash",
]
