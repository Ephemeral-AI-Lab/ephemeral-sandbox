"""Bundle helper + idempotent uploader for the in-sandbox CI runtime.

The bundle is a tar.gz containing the minimal set of project + vendored
modules needed to run ``python -m sandbox.code_intelligence.daemon``
inside a sandbox: the entire ``sandbox/code_intelligence/`` tree, the
transitive ``sandbox.api``/``sandbox.client.async_bridge``/``sandbox.lifecycle.commit``
imports it pulls in, plus a vendored pure-Python ``msgpack/`` so the
sandbox image does not need ``pip install``.

Phase 0 already added ``msgpack>=1.0.0`` to ``[project.dependencies]`` so the
vendored copy is sourced from the orchestrator's own venv at bundle-build
time.

The companion :func:`ensure_runtime_uploaded` extracts the bundle under
``/tmp/eos-ci-runtime/`` once per ``(transport, sandbox_id)`` pair; subsequent
calls no-op when the previously-recorded ``.bundle-hash`` marker matches.
"""

from __future__ import annotations

import asyncio
import gzip
import hashlib
import io
import logging
import os
import shlex
import tarfile
from pathlib import Path

from sandbox.api.transport import SandboxTransport

__all__ = [
    "BUNDLE_REMOTE_DIR",
    "DaemonUnavailable",
    "DaemonLauncher",
    "remote_state_dir",
    "ensure_runtime_uploaded",
    "_runtime_bundle_bytes",
]

logger = logging.getLogger(__name__)

BUNDLE_REMOTE_DIR = "/tmp/eos-ci-runtime"
"""Remote directory the bundle is extracted into."""

_BUNDLE_HASH_MARKER = f"{BUNDLE_REMOTE_DIR}/.bundle-hash"


def _src_root() -> Path:
    """Return the orchestrator's ``backend/src/`` directory.

    ``__file__`` is at
    ``backend/src/sandbox/code_intelligence/daemon/launcher.py``, so four
    ``.parent`` hops climb back up to ``backend/src/``.
    """
    return Path(__file__).resolve().parent.parent.parent.parent


def _msgpack_dir() -> Path:
    """Locate the orchestrator's installed ``msgpack/`` package."""
    import msgpack  # noqa: PLC0415 — lazy: only used at bundle-build time.

    return Path(msgpack.__file__).resolve().parent


def _is_excluded(path: Path) -> bool:
    parts = set(path.parts)
    return "__pycache__" in parts or path.suffix in {".pyc", ".pyo"}


def _normalize_tarinfo(info: tarfile.TarInfo) -> tarfile.TarInfo:
    """Strip per-environment metadata so the bundle hashes deterministically."""
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    # Mode normalization keeps the bundle hash stable across orchestrators
    # with different umasks / filesystem ACLs. We bundle .py source files
    # only, so 0o644 is the right canonical mode.
    info.mode = 0o644
    return info


_BUNDLE_CACHE: bytes | None = None


def _runtime_bundle_bytes() -> bytes:
    """Build the in-sandbox runtime bundle as a gzip tarball.

    The result is memoized per orchestrator process — the bundle builds
    deterministically from on-disk source, and rebuilding on every call
    dominates the warm-bundle-upload SLO (the marker check completes in
    ~300 ms but the tarball build is multiple seconds of disk + gzip).

    Layout (inside the tarball):

    * ``msgpack/**/*.py``                                    (vendored, pure Python)
    * ``sandbox/__init__.py`` + ``sandbox/errors.py``
    * ``sandbox/api/**/*.py``
    * ``sandbox/client/__init__.py`` + ``sandbox/client/async_bridge.py``
    * ``sandbox/lifecycle/__init__.py`` + ``sandbox/lifecycle/commit.py``
    * ``sandbox/code_intelligence/**/*.py``                  (full tree)
    """
    global _BUNDLE_CACHE
    if _BUNDLE_CACHE is not None:
        return _BUNDLE_CACHE

    src = _src_root()
    sandbox_dir = src / "sandbox"
    msgpack_dir = _msgpack_dir()

    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as tar:
        # --- Vendored msgpack (pure-Python only, skip compiled .so) -----------
        msgpack_parent = msgpack_dir.parent
        for path in sorted(msgpack_dir.rglob("*.py")):
            if _is_excluded(path):
                continue
            tar.add(
                path,
                arcname=path.relative_to(msgpack_parent).as_posix(),
                filter=_normalize_tarinfo,
            )

        # --- sandbox/ root --------------------------------------------------
        for filename in ("__init__.py", "errors.py"):
            p = sandbox_dir / filename
            if p.exists():
                tar.add(p, arcname=f"sandbox/{filename}", filter=_normalize_tarinfo)

        # --- sandbox/api -----------------------------------------------------
        for path in sorted((sandbox_dir / "api").rglob("*.py")):
            if _is_excluded(path):
                continue
            tar.add(
                path,
                arcname=f"sandbox/{path.relative_to(sandbox_dir).as_posix()}",
                filter=_normalize_tarinfo,
            )

        # --- sandbox/client (only async_bridge + __init__) -------------------
        client_dir = sandbox_dir / "client"
        for filename in ("__init__.py", "async_bridge.py"):
            p = client_dir / filename
            if p.exists():
                tar.add(
                    p,
                    arcname=f"sandbox/client/{filename}",
                    filter=_normalize_tarinfo,
                )

        # --- sandbox/lifecycle (only __init__ + commit) ----------------------
        lifecycle_dir = sandbox_dir / "lifecycle"
        for filename in ("__init__.py", "commit.py"):
            p = lifecycle_dir / filename
            if p.exists():
                tar.add(
                    p,
                    arcname=f"sandbox/lifecycle/{filename}",
                    filter=_normalize_tarinfo,
                )

        # --- sandbox/code_intelligence (full tree) ---------------------------
        ci_dir = sandbox_dir / "code_intelligence"
        for path in sorted(ci_dir.rglob("*.py")):
            if _is_excluded(path):
                continue
            tar.add(
                path,
                arcname=f"sandbox/{path.relative_to(sandbox_dir).as_posix()}",
                filter=_normalize_tarinfo,
            )

    # Gzip with a fixed mtime so the bundle hash is deterministic — required
    # for the .bundle-hash idempotency check.
    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode="wb", mtime=0) as gz:
        gz.write(raw.getvalue())
    _BUNDLE_CACHE = compressed.getvalue()
    return _BUNDLE_CACHE


_BUNDLE_HASH_CACHE: str | None = None


def bundle_hash(bundle: bytes | None = None) -> str:
    """Stable hex digest of the runtime bundle (used for upload idempotency).

    Memoized per-process when the caller doesn't pass an explicit bundle —
    the bundle is itself memoized via :data:`_BUNDLE_CACHE`, so the hash is
    only computed once per orchestrator lifecycle. This collapses the
    ``ensure_runtime_uploaded`` warm path from ~5 s of SHA-256-on-100KB
    work to a dictionary lookup.
    """
    global _BUNDLE_HASH_CACHE
    if bundle is None:
        if _BUNDLE_HASH_CACHE is not None:
            return _BUNDLE_HASH_CACHE
        bundle = _runtime_bundle_bytes()
        _BUNDLE_HASH_CACHE = hashlib.sha256(bundle).hexdigest()
        return _BUNDLE_HASH_CACHE
    return hashlib.sha256(bundle).hexdigest()


_BUNDLE_REMOTE_TARBALL = f"{BUNDLE_REMOTE_DIR}/bundle.tar.gz"

# Each base64 chunk we ship via a single ``exec`` call. Daytona's exec
# pathway rejects very large argv strings; 32 KB per chunk fits inside
# every observed limit and keeps the upload to <10 round-trips for a
# ~100 KB bundle. The matching memory:
# `'checked batch apply failed' = argv E2BIG`.
#
# 32 KB is divisible by 4, so every chunk is a 4-aligned base64 segment
# that ``base64 -d`` can decode independently — we therefore pipe each
# chunk through ``base64 -d`` and append the decoded bytes straight to
# the tarball, skipping the staged ``.b64`` intermediate file the earlier
# implementation kept around.
_CHUNK_SIZE = 32 * 1024


class DaemonUnavailable(Exception):
    """Raised when the in-sandbox daemon cannot be reached or started."""


def remote_state_dir(home: str, workspace_root: str) -> str:
    """Return the daemon state dir path as seen inside the sandbox."""
    from sandbox.code_intelligence.daemon.storage import (
        workspace_root_hash,
    )

    home = str(home or "").rstrip("/") or "/root"
    return f"{home}/.cache/eos-ci/{workspace_root_hash(workspace_root)}/v1"


async def ensure_runtime_uploaded(
    transport: SandboxTransport, sandbox_id: str
) -> str:
    """Upload the runtime bundle to ``/tmp/eos-ci-runtime/`` if needed.

    Idempotent: when ``.bundle-hash`` already exists with a matching digest,
    no upload occurs. Returns the bundle hash so callers can correlate logs.

    Implementation note — the bundle is streamed as **chunked base64 over
    repeated ``transport.exec`` calls**. Two earlier attempts were tried
    and rejected:

    1. Inlining base64 in a single argv string blew past ``ARG_MAX`` once
       the bundle plus shell escaping crossed ~100 KB.
    2. ``transport.write_bytes`` (Daytona ``fs.upload_file``) returned
       ``502 Bad Gateway`` from Daytona's proxy on every attempt.

    Chunked-base64 is the third approach: each chunk is small, the upload
    is incremental (so partial failures are recoverable), and it depends
    only on ``transport.exec``, which is the most reliable verb.
    """
    # Hash before bytes: hash is memoized per-process, so the warm path can
    # short-circuit on a hit without rebuilding the tarball at all. Only
    # rebuild bytes when we're actually going to upload.
    digest = bundle_hash()
    marker_check = await transport.exec(
        sandbox_id,
        f"test -f {shlex.quote(_BUNDLE_HASH_MARKER)} && cat {shlex.quote(_BUNDLE_HASH_MARKER)}",
    )
    existing = (getattr(marker_check, "stdout", "") or "").strip()
    if getattr(marker_check, "exit_code", 1) == 0 and existing == digest:
        logger.debug(
            "ci runtime bundle already uploaded (%s) on %s", digest[:8], sandbox_id
        )
        return digest

    import base64

    bundle = _runtime_bundle_bytes()
    encoded = base64.b64encode(bundle).decode("ascii")

    # Stage: ensure dir + truncate target tarball. We append decoded bytes
    # directly into the tarball below, so no separate ``.b64`` staging
    # file is needed.
    setup = await transport.exec(
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

    # Stream each base64 chunk through ``base64 -d`` and append the decoded
    # bytes to the tarball. Each ``_CHUNK_SIZE`` slice is 4-aligned so
    # ``base64 -d`` can decode it independently.
    for i in range(0, len(encoded), _CHUNK_SIZE):
        chunk = encoded[i : i + _CHUNK_SIZE]
        write_cmd = (
            f"printf %s {shlex.quote(chunk)} | base64 -d "
            f">> {shlex.quote(_BUNDLE_REMOTE_TARBALL)}"
        )
        result = await transport.exec(sandbox_id, write_cmd, timeout=60)
        if getattr(result, "exit_code", 1) != 0:
            raise RuntimeError(
                f"runtime bundle chunk write failed at offset {i} "
                f"(sandbox={sandbox_id!r}): {getattr(result, 'stdout', '')}"
            )

    # Extract + clean up the tarball + atomically install hash marker.
    finalize_cmd = (
        f"cd {shlex.quote(BUNDLE_REMOTE_DIR)} && "
        f"tar -xzf {shlex.quote(_BUNDLE_REMOTE_TARBALL)} && "
        f"rm -f {shlex.quote(_BUNDLE_REMOTE_TARBALL)} && "
        f"printf %s {shlex.quote(digest)} > {shlex.quote(_BUNDLE_HASH_MARKER)}"
    )
    result = await transport.exec(sandbox_id, finalize_cmd, timeout=60)
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


class DaemonLauncher:
    """Spawn and supervise the sandbox-local CI daemon."""

    def __init__(
        self,
        transport: SandboxTransport,
        sandbox_id: str,
        workspace_root: str,
    ) -> None:
        self._transport = transport
        self._sandbox_id = sandbox_id
        self._workspace_root = workspace_root
        self._home_cache: str | None = None

    async def ensure_daemon(self, *, timeout_s: float = 10.0) -> None:
        """Ensure the daemon is alive and its Unix socket exists."""
        logger.info(
            "ensuring CI daemon for sandbox %s at workspace %s",
            self._sandbox_id,
            self._workspace_root,
        )
        if await self.is_alive():
            logger.info("CI daemon pid is alive for sandbox %s", self._sandbox_id)
            if await self._wait_for_socket(timeout_s=timeout_s):
                logger.info("CI daemon socket is ready for sandbox %s", self._sandbox_id)
                return
            raise DaemonUnavailable(
                f"daemon pid is alive but socket did not appear within {timeout_s:.1f}s"
            )

        logger.info("CI daemon not alive for sandbox %s; uploading runtime", self._sandbox_id)
        await ensure_runtime_uploaded(self._transport, self._sandbox_id)
        logger.info("spawning CI daemon for sandbox %s", self._sandbox_id)
        await self.spawn()
        if await self._wait_for_socket(timeout_s=timeout_s):
            logger.info("CI daemon socket became ready for sandbox %s", self._sandbox_id)
            return
        raise DaemonUnavailable(
            f"daemon socket did not become ready within {timeout_s:.1f}s"
        )

    async def is_alive(self) -> bool:
        """Return True when the pid file points at a live process."""
        pid_path = await self.pid_path()
        cmd = (
            f"test -f {shlex.quote(pid_path)} && "
            f"pid=$(cat {shlex.quote(pid_path)}) && "
            'test -n "$pid" && kill -0 "$pid"'
        )
        result = await self._transport.exec(self._sandbox_id, cmd, timeout=10)
        return getattr(result, "exit_code", 1) == 0

    async def spawn(self) -> None:
        """Launch the daemon detached from the transport exec shell."""
        state = await self.state_dir()
        log_path = f"{state}/daemon.log"
        log_level = os.environ.get("EOS_CI_DAEMON_LOG_LEVEL", "INFO")
        cmd = (
            f"mkdir -p {shlex.quote(state)} && "
            f"cd {shlex.quote(BUNDLE_REMOTE_DIR)} && "
            "setsid nohup python3 -m sandbox.code_intelligence.daemon "
            f"--workspace-root {shlex.quote(self._workspace_root)} "
            f"--log-level {shlex.quote(log_level)} "
            f">> {shlex.quote(log_path)} 2>&1 </dev/null & echo $!"
        )
        try:
            result = await self._transport.exec(self._sandbox_id, cmd, timeout=5)
        except Exception:
            logger.debug(
                "ci daemon spawn command did not return cleanly for sandbox %s; "
                "polling socket readiness before failing",
                self._sandbox_id,
                exc_info=True,
            )
            return
        if getattr(result, "exit_code", 1) != 0:
            raise DaemonUnavailable(
                f"daemon spawn failed for sandbox={self._sandbox_id!r}: "
                f"{getattr(result, 'stdout', '')}"
            )
        logger.info(
            "ci daemon spawn requested for sandbox %s (pid=%s)",
            self._sandbox_id,
            (getattr(result, "stdout", "") or "").strip(),
        )

    async def shutdown(self) -> None:
        """Ask a running daemon to terminate and wait briefly for cleanup."""
        pid_path = await self.pid_path()
        socket_path = await self.socket_path()
        cmd = (
            f"if test -f {shlex.quote(pid_path)}; then "
            f"kill -TERM $(cat {shlex.quote(pid_path)}) 2>/dev/null || true; "
            "fi"
        )
        await self._transport.exec(self._sandbox_id, cmd, timeout=10)
        deadline = asyncio.get_running_loop().time() + 5.0
        while asyncio.get_running_loop().time() < deadline:
            if not await self.is_alive() and not await self._path_is_socket(socket_path):
                return
            await asyncio.sleep(0.1)

    async def state_dir(self) -> str:
        home = await self._remote_home()
        return remote_state_dir(home, self._workspace_root)

    async def socket_path(self) -> str:
        return f"{await self.state_dir()}/daemon.sock"

    async def pid_path(self) -> str:
        return f"{await self.state_dir()}/daemon.pid"

    async def _remote_home(self) -> str:
        if self._home_cache is not None:
            return self._home_cache
        result = await self._transport.exec(self._sandbox_id, 'printf %s "$HOME"', timeout=10)
        home = (getattr(result, "stdout", "") or "").strip() or "/root"
        self._home_cache = home
        return home

    async def _wait_for_socket(self, *, timeout_s: float) -> bool:
        socket_path = await self.socket_path()
        deadline = asyncio.get_running_loop().time() + timeout_s
        while asyncio.get_running_loop().time() < deadline:
            if await self._path_is_socket(socket_path):
                return True
            await asyncio.sleep(0.1)
        return False

    async def _path_is_socket(self, path: str) -> bool:
        result = await self._transport.exec(
            self._sandbox_id,
            f"test -S {shlex.quote(path)}",
            timeout=10,
        )
        return getattr(result, "exit_code", 1) == 0
