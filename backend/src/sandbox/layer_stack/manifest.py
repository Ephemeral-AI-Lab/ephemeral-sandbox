"""Manifest contracts and storage helpers for the append-only layer stack."""

from __future__ import annotations

import hashlib
import json
import os
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

from sandbox.layer_stack.errors import ManifestConflictError


MANIFEST_SCHEMA_VERSION = 1
ACTIVE_MANIFEST_FILE = "manifest.json"
LAYERS_DIR = "layers"
STAGING_DIR = "staging"


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
        return cls(layer_id=str(payload["layer_id"]), path=str(payload["path"]))


@dataclass(frozen=True)
class Manifest:
    version: int
    layers: tuple[LayerRef, ...]
    schema_version: int = MANIFEST_SCHEMA_VERSION

    def __post_init__(self) -> None:
        if self.schema_version != MANIFEST_SCHEMA_VERSION:
            raise ManifestConflictError(
                f"unsupported manifest schema_version: {self.schema_version}"
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
        # WR-04 + WR-08: require both top-level keys explicitly so a torn
        # write that lost the `layers` key is treated as corruption, not as
        # a legitimately-empty manifest.
        if "version" not in payload:
            raise ManifestConflictError("manifest payload missing required field: version")
        if "layers" not in payload:
            raise ManifestConflictError("manifest payload missing required field: layers")
        schema_version = int(payload.get("schema_version", MANIFEST_SCHEMA_VERSION))
        if schema_version > MANIFEST_SCHEMA_VERSION:
            raise ManifestConflictError(
                "manifest schema_version is newer than this runtime supports: "
                f"{schema_version}"
            )
        if schema_version != MANIFEST_SCHEMA_VERSION:
            raise ManifestConflictError(f"unsupported manifest schema_version: {schema_version}")
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


def manifest_path(storage_root: str | Path) -> Path:
    return Path(storage_root) / ACTIVE_MANIFEST_FILE


def read_manifest(path: str | Path) -> Manifest:
    manifest_file = Path(path)
    if not manifest_file.exists():
        return empty_manifest()
    payload = json.loads(manifest_file.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError("manifest payload must be an object")
    return Manifest.from_dict(payload)


def write_manifest_atomic(path: str | Path, manifest: Manifest) -> None:
    manifest_file = Path(path)
    manifest_file.parent.mkdir(parents=True, exist_ok=True)
    tmp = manifest_file.with_name(f".{manifest_file.name}.tmp")
    data = json.dumps(manifest.to_dict(), indent=2, sort_keys=True).encode("utf-8")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    try:
        os.write(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(tmp, manifest_file)
    dir_fd = os.open(manifest_file.parent, os.O_RDONLY)
    try:
        os.fsync(dir_fd)
    finally:
        os.close(dir_fd)


class FileManifestStore:
    """Filesystem-backed active manifest store."""

    def __init__(self, storage_root: str | Path) -> None:
        self._path = manifest_path(storage_root)

    @property
    def path(self) -> Path:
        return self._path

    def read(self) -> Manifest:
        return read_manifest(self._path)

    def write(self, manifest: Manifest) -> None:
        write_manifest_atomic(self._path, manifest)


__all__ = [
    "ACTIVE_MANIFEST_FILE",
    "FileManifestStore",
    "LAYERS_DIR",
    "LayerRef",
    "MANIFEST_SCHEMA_VERSION",
    "Manifest",
    "ManifestConflictError",
    "STAGING_DIR",
    "empty_manifest",
    "manifest_path",
    "manifest_root_hash",
    "read_manifest",
    "write_manifest_atomic",
]
