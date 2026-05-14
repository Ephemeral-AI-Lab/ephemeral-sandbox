"""Manifest contracts for the append-only sandbox layer stack."""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass
from collections.abc import Mapping
from pathlib import PurePosixPath

from sandbox.layer_stack.errors import ManifestConflictError


MANIFEST_SCHEMA_VERSION = 1


@dataclass(frozen=True, order=True)
class LayerRef:
    layer_id: str
    path: str

    def __post_init__(self) -> None:
        if not self.layer_id:
            raise ValueError("layer_id must not be empty")
        if not self.path:
            raise ValueError("layer path must not be empty")
        if "\0" in self.path:
            raise ValueError(f"layer path must not contain NUL bytes: {self.path!r}")
        posix = PurePosixPath(self.path)
        if posix.is_absolute():
            raise ValueError(f"layer path must be relative: {self.path}")
        if any(part == ".." for part in posix.parts):
            raise ValueError(f"layer path must not contain '..': {self.path}")

    def to_dict(self) -> dict[str, str]:
        return {"layer_id": self.layer_id, "path": self.path}

    @classmethod
    def from_dict(cls, payload: Mapping[str, object]) -> LayerRef:
        layer_id = str(payload["layer_id"])
        path = str(payload["path"])
        return cls(layer_id=layer_id, path=path)


@dataclass(frozen=True)
class Manifest:
    version: int
    layers: tuple[LayerRef, ...]
    schema_version: int = MANIFEST_SCHEMA_VERSION

    def __post_init__(self) -> None:
        if self.schema_version != MANIFEST_SCHEMA_VERSION:
            raise ManifestConflictError(
                "unsupported manifest schema_version: "
                f"{self.schema_version}"
            )
        if self.version < 0:
            raise ValueError("manifest version must be non-negative")
        object.__setattr__(self, "layers", tuple(self.layers))

    @property
    def depth(self) -> int:
        return len(self.layers)

    def to_dict(self) -> dict[str, object]:
        return {
            "schema_version": self.schema_version,
            "version": self.version,
            "layers": [layer.to_dict() for layer in self.layers],
        }

    @classmethod
    def from_dict(cls, payload: Mapping[str, object]) -> Manifest:
        # WR-04 + WR-08: require both top-level keys explicitly. The
        # pre-fix `.get("layers", ())` would silently promote a torn write
        # (manifest.json that lost the `layers` key) into an "empty stack,
        # version N" Manifest, which downstream paths interpret as
        # legitimately-empty rather than as corruption.
        if "version" not in payload:
            raise ManifestConflictError(
                "manifest payload missing required field: version"
            )
        if "layers" not in payload:
            raise ManifestConflictError(
                "manifest payload missing required field: layers"
            )
        raw_schema_version = payload.get("schema_version", MANIFEST_SCHEMA_VERSION)
        schema_version = int(raw_schema_version)
        if schema_version > MANIFEST_SCHEMA_VERSION:
            raise ManifestConflictError(
                "manifest schema_version is newer than this runtime supports: "
                f"{schema_version}"
            )
        if schema_version != MANIFEST_SCHEMA_VERSION:
            raise ManifestConflictError(
                f"unsupported manifest schema_version: {schema_version}"
            )
        raw_layers = payload["layers"]
        if not isinstance(raw_layers, list):
            raise ValueError("manifest layers must be a list")
        layers: list[LayerRef] = []
        for item in raw_layers:
            if not isinstance(item, dict):
                raise ValueError("manifest layer entries must be objects")
            layers.append(LayerRef.from_dict(item))
        return cls(
            version=int(payload["version"]),
            layers=tuple(layers),
            schema_version=schema_version,
        )


def empty_manifest() -> Manifest:
    return Manifest(version=0, layers=())


def manifest_root_hash(manifest: Manifest) -> str:
    """Return a stable identity hash for the manifest's root view."""
    payload = {"layers": [layer.to_dict() for layer in manifest.layers]}
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()
