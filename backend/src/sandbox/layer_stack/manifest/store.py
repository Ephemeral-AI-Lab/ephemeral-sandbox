"""Manifest file layout and persistence helpers."""

from __future__ import annotations

import json
import os
from pathlib import Path

from sandbox.layer_stack.manifest.model import Manifest, empty_manifest


ACTIVE_MANIFEST_FILE = "manifest.json"
LAYERS_DIR = "layers"
STAGING_DIR = "staging"


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
    "STAGING_DIR",
    "manifest_path",
    "read_manifest",
    "write_manifest_atomic",
]
