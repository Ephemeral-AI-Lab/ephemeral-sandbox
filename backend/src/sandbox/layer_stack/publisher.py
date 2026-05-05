"""Policy-blind immutable layer publisher."""

from __future__ import annotations

import hashlib
import os
import shutil
import time
import uuid
from collections.abc import Callable, Sequence
from pathlib import Path, PurePosixPath

from sandbox.layer_stack.changes import LayerChange
from sandbox.layer_stack.lease_budget import BudgetDecision
from sandbox.layer_stack.manifest import (
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    ManifestConflictError,
    read_manifest,
    write_manifest_atomic,
)
from sandbox.layer_stack.merged_view import OPAQUE_MARKER, WHITEOUT_PREFIX


class CommitBackpressureError(RuntimeError):
    """Raised when lease pressure blocks publishing a new layer."""

    def __init__(self, decision: BudgetDecision) -> None:
        super().__init__(decision.reason)
        self.decision = decision


class LayerPublisher:
    """Writes accepted changes into immutable layers and publishes manifests."""

    def __init__(
        self,
        storage_root: str | Path,
        manifest_file: str | Path,
        *,
        id_factory: Callable[[int], str] | None = None,
        backpressure_checker: Callable[[Manifest], BudgetDecision] | None = None,
    ) -> None:
        self._storage_root = Path(storage_root)
        self._manifest_file = Path(manifest_file)
        self._id_factory = id_factory or _default_layer_id
        self._backpressure_checker = backpressure_checker

    def publish_layer_locked(
        self,
        changes: Sequence[LayerChange],
        *,
        expected_manifest: Manifest,
        timings: dict[str, float] | None = None,
    ) -> Manifest:
        total_start = time.perf_counter()
        read_active_start = time.perf_counter()
        active = read_manifest(self._manifest_file)
        _record(
            timings,
            "layer_stack.publish.read_active_manifest_s",
            time.perf_counter() - read_active_start,
        )
        if active != expected_manifest:
            raise ManifestConflictError(
                "active manifest changed before layer publish: "
                f"expected version {expected_manifest.version}, "
                f"found version {active.version}"
            )
        if not changes:
            _record(
                timings,
                "layer_stack.publish.total_s",
                time.perf_counter() - total_start,
            )
            return active

        digest_start = time.perf_counter()
        layer_digest = _changes_digest(changes)
        if _head_layer_digest(self._storage_root, active) == layer_digest:
            _record(
                timings,
                "layer_stack.publish.digest_check_s",
                time.perf_counter() - digest_start,
            )
            _record(
                timings,
                "layer_stack.publish.idempotent_s",
                time.perf_counter() - total_start,
            )
            _record(
                timings,
                "layer_stack.publish.total_s",
                time.perf_counter() - total_start,
            )
            return active
        _record(
            timings,
            "layer_stack.publish.digest_check_s",
            time.perf_counter() - digest_start,
        )

        backpressure_start = time.perf_counter()
        self._check_backpressure(active)
        _record(
            timings,
            "layer_stack.publish.check_backpressure_s",
            time.perf_counter() - backpressure_start,
        )
        allocate_start = time.perf_counter()
        layer_id, staging_dir, layer_dir = self._allocate_layer_paths(active.version + 1)
        _record(
            timings,
            "layer_stack.publish.allocate_layer_paths_s",
            time.perf_counter() - allocate_start,
        )
        create_staging_start = time.perf_counter()
        staging_dir.mkdir(parents=True)
        _record(
            timings,
            "layer_stack.publish.create_staging_s",
            time.perf_counter() - create_staging_start,
        )
        try:
            write_changes_start = time.perf_counter()
            for change in changes:
                self._write_change(staging_dir, change)
            _record(
                timings,
                "layer_stack.publish.write_changes_s",
                time.perf_counter() - write_changes_start,
            )
            replace_start = time.perf_counter()
            layer_dir.parent.mkdir(parents=True, exist_ok=True)
            os.replace(staging_dir, layer_dir)
            _write_layer_digest(self._storage_root, layer_id, layer_digest)
            _record(
                timings,
                "layer_stack.publish.replace_staging_s",
                time.perf_counter() - replace_start,
            )
        except Exception:
            shutil.rmtree(staging_dir, ignore_errors=True)
            raise

        new_manifest = Manifest(
            version=active.version + 1,
            layers=(
                LayerRef(layer_id=layer_id, path=f"{LAYERS_DIR}/{layer_id}"),
                *active.layers,
            ),
        )
        read_latest_start = time.perf_counter()
        latest = read_manifest(self._manifest_file)
        _record(
            timings,
            "layer_stack.publish.read_latest_manifest_s",
            time.perf_counter() - read_latest_start,
        )
        if latest != active:
            _remove_path(layer_dir)
            _digest_path(self._storage_root, layer_id).unlink(missing_ok=True)
            raise ManifestConflictError(
                "active manifest changed during layer publish: "
                f"expected version {active.version}, found version {latest.version}"
            )
        write_manifest_start = time.perf_counter()
        try:
            write_manifest_atomic(self._manifest_file, new_manifest)
        except Exception:
            _remove_path(layer_dir)
            _digest_path(self._storage_root, layer_id).unlink(missing_ok=True)
            raise
        _record(
            timings,
            "layer_stack.publish.write_manifest_s",
            time.perf_counter() - write_manifest_start,
        )
        _record(
            timings,
            "layer_stack.publish.total_s",
            time.perf_counter() - total_start,
        )
        return new_manifest

    def _check_backpressure(self, active: Manifest) -> None:
        if self._backpressure_checker is None:
            return
        decision = self._backpressure_checker(active)
        if decision.kind == "backpressure_commits":
            raise CommitBackpressureError(decision)

    def _allocate_layer_paths(self, next_version: int) -> tuple[str, Path, Path]:
        for _ in range(100):
            layer_id = self._id_factory(next_version)
            layer_dir = self._storage_root / LAYERS_DIR / layer_id
            staging_dir = self._storage_root / STAGING_DIR / f"{layer_id}.staging"
            if not layer_dir.exists() and not staging_dir.exists():
                return layer_id, staging_dir, layer_dir
        raise RuntimeError("could not allocate a unique layer id")

    def _write_change(self, layer_dir: Path, change: LayerChange) -> None:
        if change.kind == "write":
            self._write_file(layer_dir, change)
        elif change.kind == "delete":
            _whiteout_path(layer_dir, change.path).write_text("", encoding="utf-8")
        elif change.kind == "symlink":
            self._write_symlink(layer_dir, change)
        elif change.kind == "opaque_dir":
            marker = _join_rel(layer_dir, change.path) / OPAQUE_MARKER
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.write_text("", encoding="utf-8")
        else:
            raise ValueError(f"unsupported layer change kind: {change.kind}")

    def _write_file(self, layer_dir: Path, change: LayerChange) -> None:
        if change.source_path is None:
            raise ValueError("write changes require source_path")
        source = Path(change.source_path)
        content = source.read_bytes()
        if change.content_hash and hashlib.sha256(content).hexdigest() != change.content_hash:
            raise ValueError(f"content hash mismatch for {change.path}")
        target = _join_rel(layer_dir, change.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        _remove_path(target)
        target.write_bytes(content)

    def _write_symlink(self, layer_dir: Path, change: LayerChange) -> None:
        if change.source_path is None:
            raise ValueError("symlink changes require source_path target")
        target = _join_rel(layer_dir, change.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        os.symlink(change.source_path, target)


def _default_layer_id(next_version: int) -> str:
    return f"L{next_version:06d}-{uuid.uuid4().hex[:8]}"


def _changes_digest(changes: Sequence[LayerChange]) -> str:
    digest = hashlib.sha256()
    for change in changes:
        digest.update(change.kind.encode("utf-8"))
        digest.update(b"\0")
        digest.update(change.path.encode("utf-8"))
        digest.update(b"\0")
        if change.kind == "write":
            if change.source_path is None:
                raise ValueError("write changes require source_path")
            digest.update(Path(change.source_path).read_bytes())
        elif change.kind == "symlink":
            digest.update(str(change.source_path or "").encode("utf-8"))
        elif change.content_hash:
            digest.update(change.content_hash.encode("utf-8"))
        digest.update(b"\0")
    return digest.hexdigest()


def _metadata_dir(storage_root: Path) -> Path:
    return storage_root / ".layer-metadata"


def _digest_path(storage_root: Path, layer_id: str) -> Path:
    return _metadata_dir(storage_root) / f"{layer_id}.digest"


def _write_layer_digest(storage_root: Path, layer_id: str, digest: str) -> None:
    metadata = _metadata_dir(storage_root)
    metadata.mkdir(parents=True, exist_ok=True)
    _digest_path(storage_root, layer_id).write_text(digest, encoding="utf-8")


def _head_layer_digest(storage_root: Path, active: Manifest) -> str | None:
    if not active.layers:
        return None
    try:
        return _digest_path(storage_root, active.layers[0].layer_id).read_text(
            encoding="utf-8",
        )
    except OSError:
        return None


def _join_rel(root: Path, rel: str) -> Path:
    return root.joinpath(*PurePosixPath(rel).parts)


def _whiteout_path(layer_dir: Path, rel: str) -> Path:
    target = PurePosixPath(rel)
    parent_parts = tuple(part for part in target.parent.parts if part != ".")
    whiteout = layer_dir.joinpath(*parent_parts, f"{WHITEOUT_PREFIX}{target.name}")
    whiteout.parent.mkdir(parents=True, exist_ok=True)
    return whiteout


def _remove_path(path: Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink(missing_ok=True)
    elif path.is_dir():
        shutil.rmtree(path)


def _record(timings: dict[str, float] | None, key: str, value: float) -> None:
    if timings is not None:
        timings[key] = value
