"""Bundle helper + idempotent uploader for the sandbox-local runtime.

The bundle is a tar.gz containing the project modules needed to import
the deployed runtime server and setup orchestrator contract inside a sandbox.
This module is host-side bootstrap code;
bundle upload goes through ``sandbox.api.raw_exec`` by sandbox id.
"""

from __future__ import annotations

import base64
import gzip
import hashlib
import io
import logging
import shlex
import tarfile
from pathlib import Path
from typing import Protocol

from sandbox.api.models import RawExecResult
from sandbox.api.raw_exec import raw_exec

__all__ = [
    "BUNDLE_REMOTE_DIR",
    "bundle_hash",
    "ensure_runtime_uploaded",
    "_ensure_runtime_uploaded_with_exec",
    "_runtime_bundle_bytes",
]

logger = logging.getLogger(__name__)

BUNDLE_REMOTE_DIR = "/tmp/eos-ci-runtime"
"""Remote directory the bundle is extracted into."""

_RUNTIME_EXCLUDE_PARTS = {
    "_server_dispatch.py",
    "backends",
    "command_client.py",
    "registry.py",
    "service.py",
    "shell_command_executor.py",
}
_OCC_EXCLUDE_PARTS = {"client.py"}
_OVERLAY_EXCLUDE_PARTS = {"client.py"}

_BUNDLE_HASH_MARKER = f"{BUNDLE_REMOTE_DIR}/.bundle-hash"
_BUNDLE_REMOTE_TARBALL = f"{BUNDLE_REMOTE_DIR}/bundle.tar.gz"

# Keep chunks below observed Daytona argv limits while preserving base64
# alignment so each chunk decodes independently.
_CHUNK_SIZE = 32 * 1024


class _RawExecCallable(Protocol):
    async def __call__(
        self,
        sandbox_id: str,
        command: str,
        *,
        timeout: int | None = None,
    ) -> RawExecResult: ...


def _src_root() -> Path:
    """Return the orchestrator's ``backend/src/`` directory."""
    return Path(__file__).resolve().parent.parent.parent


def _is_excluded(path: Path) -> bool:
    parts = set(path.parts)
    if "__pycache__" in parts or path.suffix in {".pyc", ".pyo"}:
        return True
    return path.name in {"bundle.py", "raw_exec.py"}


def _normalize_tarinfo(info: tarfile.TarInfo) -> tarfile.TarInfo:
    """Strip per-environment metadata so the bundle hashes deterministically."""
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mode = 0o644
    return info


def _add_if_exists(tar: tarfile.TarFile, path: Path, *, arcname: str) -> None:
    if path.exists():
        tar.add(path, arcname=arcname, filter=_normalize_tarinfo)


def _add_python_tree(
    tar: tarfile.TarFile,
    root: Path,
    *,
    sandbox_dir: Path,
    exclude_parts: set[str] | None = None,
) -> None:
    excluded = exclude_parts or set()
    for path in sorted(root.rglob("*.py")):
        if _is_excluded(path):
            continue
        rel = path.relative_to(root)
        if excluded.intersection(rel.parts):
            continue
        tar.add(
            path,
            arcname=f"sandbox/{path.relative_to(sandbox_dir).as_posix()}",
            filter=_normalize_tarinfo,
        )


def _add_peer_setup_scripts(tar: tarfile.TarFile, *, sandbox_dir: Path) -> None:
    for path in sorted(sandbox_dir.rglob("setup.sh")):
        if "__pycache__" in path.parts:
            continue
        tar.add(
            path,
            arcname=f"sandbox/{path.relative_to(sandbox_dir).as_posix()}",
            filter=_normalize_tarinfo,
        )


_BUNDLE_CACHE: bytes | None = None


def _runtime_bundle_bytes() -> bytes:
    """Build the in-sandbox runtime bundle as a gzip tarball."""
    global _BUNDLE_CACHE
    if _BUNDLE_CACHE is not None:
        return _BUNDLE_CACHE

    src = _src_root()
    sandbox_dir = src / "sandbox"
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as tar:
        for filename in ("__init__.py", "errors.py"):
            _add_if_exists(
                tar,
                sandbox_dir / filename,
                arcname=f"sandbox/{filename}",
            )

        _add_if_exists(
            tar,
            sandbox_dir / "api" / "models.py",
            arcname="sandbox/api/models.py",
        )

        client_dir = sandbox_dir / "client"
        for filename in ("__init__.py", "async_bridge.py"):
            _add_if_exists(
                tar,
                client_dir / filename,
                arcname=f"sandbox/client/{filename}",
            )

        runtime_dir = sandbox_dir / "runtime"
        _add_python_tree(
            tar,
            runtime_dir,
            sandbox_dir=sandbox_dir,
            exclude_parts=_RUNTIME_EXCLUDE_PARTS,
        )

        occ_dir = sandbox_dir / "occ"
        _add_python_tree(
            tar,
            occ_dir,
            sandbox_dir=sandbox_dir,
            exclude_parts=_OCC_EXCLUDE_PARTS,
        )

        overlay_dir = sandbox_dir / "overlay"
        _add_python_tree(
            tar,
            overlay_dir,
            sandbox_dir=sandbox_dir,
            exclude_parts=_OVERLAY_EXCLUDE_PARTS,
        )

        _add_peer_setup_scripts(tar, sandbox_dir=sandbox_dir)

    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode="wb", mtime=0) as gz:
        gz.write(raw.getvalue())
    _BUNDLE_CACHE = compressed.getvalue()
    return _BUNDLE_CACHE


_BUNDLE_HASH_CACHE: str | None = None


def bundle_hash(bundle: bytes | None = None) -> str:
    """Stable hex digest of the runtime bundle."""
    global _BUNDLE_HASH_CACHE
    if bundle is None:
        if _BUNDLE_HASH_CACHE is not None:
            return _BUNDLE_HASH_CACHE
        bundle = _runtime_bundle_bytes()
        _BUNDLE_HASH_CACHE = hashlib.sha256(bundle).hexdigest()
        return _BUNDLE_HASH_CACHE
    return hashlib.sha256(bundle).hexdigest()


async def ensure_runtime_uploaded(sandbox_id: str) -> str:
    """Upload the runtime bundle through ``sandbox.api.raw_exec`` if needed."""
    return await _ensure_runtime_uploaded_with_exec(sandbox_id, raw_exec)


async def _ensure_runtime_uploaded_with_exec(
    sandbox_id: str,
    exec_fn: _RawExecCallable,
) -> str:
    """Upload the runtime bundle using the provided un-guarded exec function."""
    digest = bundle_hash()
    marker_check = await exec_fn(
        sandbox_id,
        f"test -f {shlex.quote(_BUNDLE_HASH_MARKER)} && cat {shlex.quote(_BUNDLE_HASH_MARKER)}",
    )
    existing = (getattr(marker_check, "stdout", "") or "").strip()
    if getattr(marker_check, "exit_code", 1) == 0 and existing == digest:
        logger.debug(
            "ci runtime bundle already uploaded (%s) on %s", digest[:8], sandbox_id
        )
        return digest

    bundle = _runtime_bundle_bytes()
    encoded = base64.b64encode(bundle).decode("ascii")

    setup = await exec_fn(
        sandbox_id,
        (
            f"mkdir -p {shlex.quote(BUNDLE_REMOTE_DIR)} && "
            f": > {shlex.quote(_BUNDLE_REMOTE_TARBALL)}"
        ),
        timeout=30,
    )
    if getattr(setup, "exit_code", 1) != 0:
        raise RuntimeError(
            f"runtime bundle staging mkdir failed (sandbox={sandbox_id!r}): "
            f"{getattr(setup, 'stdout', '')}"
        )

    for i in range(0, len(encoded), _CHUNK_SIZE):
        chunk = encoded[i : i + _CHUNK_SIZE]
        write_cmd = (
            f"printf %s {shlex.quote(chunk)} | base64 -d "
            f">> {shlex.quote(_BUNDLE_REMOTE_TARBALL)}"
        )
        result = await exec_fn(sandbox_id, write_cmd, timeout=60)
        if getattr(result, "exit_code", 1) != 0:
            raise RuntimeError(
                f"runtime bundle chunk write failed at offset {i} "
                f"(sandbox={sandbox_id!r}): {getattr(result, 'stdout', '')}"
            )

    finalize_cmd = (
        f"cd {shlex.quote(BUNDLE_REMOTE_DIR)} && "
        f"tar -xzf {shlex.quote(_BUNDLE_REMOTE_TARBALL)} && "
        f"rm -f {shlex.quote(_BUNDLE_REMOTE_TARBALL)} && "
        f"printf %s {shlex.quote(digest)} > {shlex.quote(_BUNDLE_HASH_MARKER)}"
    )
    result = await exec_fn(sandbox_id, finalize_cmd, timeout=60)
    if getattr(result, "exit_code", 1) != 0:
        raise RuntimeError(
            f"runtime bundle upload failed (sandbox={sandbox_id!r}): "
            f"{getattr(result, 'stdout', '')}"
        )
    logger.info(
        "ci runtime bundle uploaded (%d bytes, %d chunks, sha=%s) to %s",
        len(bundle),
        (len(encoded) + _CHUNK_SIZE - 1) // _CHUNK_SIZE,
        digest[:8],
        sandbox_id,
    )
    return digest
