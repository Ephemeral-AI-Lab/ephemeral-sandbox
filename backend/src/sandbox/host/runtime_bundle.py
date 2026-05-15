"""Bundle helper + idempotent uploader for the sandbox-local runtime.

The bundle is a tar.gz containing the project modules needed to import
the deployed runtime server and setup orchestrator contract inside a sandbox.
This module is host-side bootstrap code; bundle upload uses the registered
provider adapter's raw exec primitive by sandbox id.
"""

from __future__ import annotations

import gzip
import hashlib
import io
import logging
import shlex
import tarfile
import uuid
from pathlib import Path

from sandbox.daemon_paths import (
    BUNDLE_HASH_MARKER as _BUNDLE_HASH_MARKER,
    BUNDLE_REMOTE_DIR as _BUNDLE_REMOTE_DIR,
    BUNDLE_REMOTE_TARBALL as _BUNDLE_REMOTE_TARBALL,
)
from sandbox.host.chunked_upload import RawExecCallable, write_base64_chunks
from sandbox.provider.registry import get_adapter

__all__ = [
    "bundle_hash",
    "clear_bundle_caches",
    "compute_bundle_hash",
    "ensure_runtime_uploaded",
    "_ensure_runtime_uploaded_with_exec",
    "_runtime_bundle_bytes",
]

logger = logging.getLogger(__name__)

def _src_root() -> Path:
    """Return the orchestrator's ``backend/src/`` directory."""
    return Path(__file__).resolve().parent.parent.parent


def _is_excluded(path: Path) -> bool:
    parts = set(path.parts)
    if "__pycache__" in parts or path.suffix in {".pyc", ".pyo"}:
        return True
    return path.name in {"runtime_bundle.py", "raw_exec.py"}


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
) -> None:
    for path in sorted(root.rglob("*.py")):
        if _is_excluded(path):
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


def _add_runtime_scripts(tar: tarfile.TarFile, *, sandbox_dir: Path) -> None:
    scripts_dir = sandbox_dir / "daemon" / "scripts"
    if not scripts_dir.exists():
        return
    for path in sorted(scripts_dir.iterdir()):
        if path.is_dir() or "__pycache__" in path.parts:
            continue
        tar.add(
            path,
            arcname=f"sandbox/{path.relative_to(sandbox_dir).as_posix()}",
            filter=_normalize_tarinfo,
        )


def _vendor_pathspec(tar: tarfile.TarFile) -> None:
    """Add the host's installed ``pathspec`` package to the bundle.

    The runtime gitignore oracle requires ``pathspec``. Without vendoring, the
    sandbox image would need a ``pip install pathspec`` step; vendoring keeps
    the runtime self-contained and fail-closed when the host dependency is
    missing.
    """
    import pathspec as _pathspec  # noqa: F401

    pkg_root = Path(_pathspec.__file__).resolve().parent
    for path in sorted(pkg_root.rglob("*.py")):
        if "__pycache__" in path.parts:
            continue
        rel = path.relative_to(pkg_root)
        tar.add(
            path,
            arcname=f"pathspec/{rel.as_posix()}",
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
        for name in (
            "__init__.py",
            "timing_keys.py",
            "daemon_paths.py",
        ):
            _add_if_exists(tar, sandbox_dir / name, arcname=f"sandbox/{name}")

        shared_dir = sandbox_dir / "_shared"
        _add_python_tree(
            tar,
            shared_dir,
            sandbox_dir=sandbox_dir,
        )

        daemon_dir = sandbox_dir / "daemon"
        _add_python_tree(
            tar,
            daemon_dir,
            sandbox_dir=sandbox_dir,
        )

        execution_dir = sandbox_dir / "execution"
        _add_python_tree(
            tar,
            execution_dir,
            sandbox_dir=sandbox_dir,
        )

        layer_stack_dir = sandbox_dir / "layer_stack"
        _add_python_tree(
            tar,
            layer_stack_dir,
            sandbox_dir=sandbox_dir,
        )

        occ_dir = sandbox_dir / "occ"
        _add_python_tree(
            tar,
            occ_dir,
            sandbox_dir=sandbox_dir,
        )

        # Bundle only the in-sandbox parts of sandbox/plugin/ — install.py
        # and session.py are host-only (they import from sandbox.host and
        # sandbox.provider). The daemon imports sandbox.plugin.runtime,
        # sandbox.plugin.handler, and sandbox.plugin.projection.
        plugin_dir = sandbox_dir / "plugin"
        for name in (
            "__init__.py",
            "handler.py",
            "op_context.py",
            "op_registry.py",
            "projection.py",
        ):
            _add_if_exists(
                tar,
                plugin_dir / name,
                arcname=f"sandbox/plugin/{name}",
            )
        plugin_runtime_dir = plugin_dir / "runtime"
        _add_python_tree(
            tar,
            plugin_runtime_dir,
            sandbox_dir=sandbox_dir,
        )

        _add_peer_setup_scripts(tar, sandbox_dir=sandbox_dir)
        _add_runtime_scripts(tar, sandbox_dir=sandbox_dir)

        _vendor_pathspec(tar)

    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode="wb", mtime=0) as gz:
        gz.write(raw.getvalue())
    _BUNDLE_CACHE = compressed.getvalue()
    return _BUNDLE_CACHE


_BUNDLE_HASH_CACHE: str | None = None


def compute_bundle_hash(bundle: bytes) -> str:
    """Pure stable hex digest helper for a concrete runtime bundle."""
    return hashlib.sha256(bundle).hexdigest()


def bundle_hash() -> str:
    """Cached stable hex digest of the default runtime bundle."""
    global _BUNDLE_HASH_CACHE
    if _BUNDLE_HASH_CACHE is not None:
        return _BUNDLE_HASH_CACHE
    _BUNDLE_HASH_CACHE = compute_bundle_hash(_runtime_bundle_bytes())
    return _BUNDLE_HASH_CACHE


def clear_bundle_caches() -> None:
    """Clear process-local runtime bundle caches. Test seam."""
    global _BUNDLE_CACHE, _BUNDLE_HASH_CACHE
    _BUNDLE_CACHE = None
    _BUNDLE_HASH_CACHE = None


async def ensure_runtime_uploaded(sandbox_id: str) -> str:
    """Upload the runtime bundle through the registered provider if needed."""
    return await _ensure_runtime_uploaded_with_exec(
        sandbox_id,
        get_adapter(sandbox_id).exec,
    )


async def _ensure_runtime_uploaded_with_exec(
    sandbox_id: str,
    exec_fn: RawExecCallable,
) -> str:
    """Upload the runtime bundle using the provided un-guarded exec function."""
    digest = bundle_hash()
    marker_check = await exec_fn(
        sandbox_id,
        f"test -f {shlex.quote(_BUNDLE_HASH_MARKER)} && cat {shlex.quote(_BUNDLE_HASH_MARKER)}",
    )
    existing = (getattr(marker_check, "stdout", "") or "").strip()
    if _exit_code(marker_check) == 0 and existing == digest:
        logger.debug(
            "sandbox runtime bundle already uploaded (%s) on %s", digest[:8], sandbox_id
        )
        return digest

    bundle = _runtime_bundle_bytes()
    staging_tarball = f"{_BUNDLE_REMOTE_TARBALL}.{uuid.uuid4().hex}.staging"

    setup = await exec_fn(
        sandbox_id,
        (
            f"mkdir -p {shlex.quote(_BUNDLE_REMOTE_DIR)} && "
            f": > {shlex.quote(staging_tarball)}"
        ),
        timeout=30,
    )
    if _exit_code(setup) != 0:
        raise RuntimeError(
            f"runtime bundle staging mkdir failed (sandbox={sandbox_id!r}): "
            f"{getattr(setup, 'stdout', '')}"
        )

    chunk_count = await write_base64_chunks(
        exec_fn,
        sandbox_id,
        content=bundle,
        remote_path=staging_tarball,
        check_result=_check_chunk_write,
        failure_message=lambda offset: (
            f"runtime bundle chunk write failed at offset {offset} "
            f"(sandbox={sandbox_id!r})"
        ),
    )

    finalize_cmd = (
        f"cd {shlex.quote(_BUNDLE_REMOTE_DIR)} && "
        f"tar -xzf {shlex.quote(staging_tarball)} && "
        f"rm -f {shlex.quote(staging_tarball)} && "
            f"printf %s {shlex.quote(digest)} > {shlex.quote(_BUNDLE_HASH_MARKER)}"
    )
    result = await exec_fn(sandbox_id, finalize_cmd, timeout=60)
    if _exit_code(result) != 0:
        raise RuntimeError(
            f"runtime bundle upload failed (sandbox={sandbox_id!r}): "
            f"{getattr(result, 'stdout', '')}"
        )
    logger.info(
        "sandbox runtime bundle uploaded (%d bytes, %d chunks, sha=%s) to %s",
        len(bundle),
        chunk_count,
        digest[:8],
        sandbox_id,
    )
    return digest


def _check_chunk_write(result: object, message: str) -> None:
    if _exit_code(result) != 0:
        raise RuntimeError(f"{message}: {getattr(result, 'stdout', '')}")


def _exit_code(result: object) -> int:
    raw = getattr(result, "exit_code", None)
    if raw is None:
        raise RuntimeError(
            f"runtime bundle exec result is missing exit_code: {type(result).__name__}"
        )
    try:
        return int(raw)
    except (TypeError, ValueError) as exc:
        raise RuntimeError(
            f"runtime bundle exec result has invalid exit_code: {raw!r}"
        ) from exc
