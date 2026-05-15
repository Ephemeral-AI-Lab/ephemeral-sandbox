"""Idempotent host-side plugin bundle uploader + setup.sh runner.

Mirrors :mod:`sandbox.host.runtime_bundle` but per-plugin: bundles
``plugin.md`` + ``tools/`` + optional ``runtime/`` + ``setup.sh`` from the
host catalog into a gzip tarball, uploads it to
``/tmp/eos-sandbox-runtime/plugins/catalog/<name>/`` on first call, runs
``setup.sh`` once, and writes a ``.installed-<hash>`` marker so subsequent
calls are cheap.

Concurrency: per ``(sandbox_id, plugin_name)`` async lock so concurrent
first-callers share a single upload + setup cycle.
"""

from __future__ import annotations

import asyncio
import base64
import gzip
import hashlib
import io
import logging
import os
import re
import shlex
import tarfile
import uuid
from pathlib import Path
from typing import Any, Protocol

from plugins.core.discovery import DEFAULT_CATALOG_DIR
from plugins.core.manifest import PluginManifest
from sandbox.daemon_paths import BUNDLE_REMOTE_DIR
from sandbox.models import RawExecResult
from sandbox.provider.registry import get_adapter

__all__ = [
    "PLUGIN_BUNDLE_REMOTE_ROOT",
    "PluginInstallError",
    "ensure_installed",
    "forget",
    "plugin_install_dir",
    "plugin_marker_path",
    "register_trusted_setup_root",
    "trusted_setup_roots",
]


logger = logging.getLogger(__name__)

# All plugins land under /tmp/eos-sandbox-runtime/plugins/catalog/<name>/.
# /tmp/eos-sandbox-runtime/ is already on the daemon's sys.path (that's how
# the in-sandbox daemon imports the runtime bundle). plugins/ and
# plugins/catalog/ are implicit namespace packages — no __init__.py is uploaded
# — so ``import plugins.catalog.<name>.runtime.server`` resolves naturally.
PLUGIN_BUNDLE_REMOTE_ROOT = f"{BUNDLE_REMOTE_DIR}/plugins/catalog"

_CHUNK_SIZE = 32 * 1024
_PLUGIN_NAME_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
# 600s headroom for plugin setup scripts that download runtime binaries or
# install small dependencies over the network while staying inside Daytona's
# exec timeout.
_DEFAULT_SETUP_TIMEOUT = 600


def _initial_trusted_setup_roots() -> set[Path]:
    """Path-based allowlist for plugins permitted to run ``setup.sh``.

    Defaults to the bundled catalog directory (where vendor plugins live). The
    env var ``EOS_PLUGIN_TRUSTED_SETUP_ROOTS`` (colon-separated absolute paths)
    extends the list — used by tests and operator-controlled installs.

    Plugins outside any trusted root must not have their ``setup.sh`` executed,
    even if discovery wires up a manifest pointing at one (see review §C1).
    """
    roots: set[Path] = {DEFAULT_CATALOG_DIR.resolve()}
    raw = os.environ.get("EOS_PLUGIN_TRUSTED_SETUP_ROOTS", "")
    for part in raw.split(":"):
        part = part.strip()
        if not part:
            continue
        try:
            roots.add(Path(part).resolve(strict=False))
        except (OSError, ValueError):
            logger.warning(
                "ignoring invalid EOS_PLUGIN_TRUSTED_SETUP_ROOTS entry: %r", part
            )
    return roots


_TRUSTED_SETUP_ROOTS: set[Path] = _initial_trusted_setup_roots()


def register_trusted_setup_root(path: str | Path) -> None:
    """Add *path* to the trusted-setup allowlist (test/operator helper)."""
    _TRUSTED_SETUP_ROOTS.add(Path(path).resolve(strict=False))


def trusted_setup_roots() -> tuple[Path, ...]:
    """Return the current trusted-setup allowlist (debugging helper)."""
    return tuple(sorted(_TRUSTED_SETUP_ROOTS))


def _is_trusted_setup_source(source_dir: Path) -> bool:
    source = source_dir.resolve(strict=False)
    for root in _TRUSTED_SETUP_ROOTS:
        try:
            source.relative_to(root)
        except ValueError:
            continue
        return True
    return False


class PluginInstallError(RuntimeError):
    """Raised when plugin install fails (upload, setup.sh, or marker write)."""


class _RawExecCallable(Protocol):
    async def __call__(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult: ...


_locks: dict[tuple[str, str], asyncio.Lock] = {}


def plugin_install_dir(plugin_name: str) -> str:
    _validate_plugin_name(plugin_name)
    return f"{PLUGIN_BUNDLE_REMOTE_ROOT}/{plugin_name}"


def plugin_marker_path(plugin_name: str, digest: str) -> str:
    return f"{plugin_install_dir(plugin_name)}/.installed-{digest}"


def _validate_plugin_name(plugin_name: str) -> None:
    if _PLUGIN_NAME_RE.fullmatch(plugin_name) is None:
        raise PluginInstallError(f"invalid plugin name: {plugin_name!r}")


async def ensure_installed(
    sandbox_id: str,
    manifest: PluginManifest,
    *,
    setup_timeout: int = _DEFAULT_SETUP_TIMEOUT,
    exec_fn: _RawExecCallable | None = None,
) -> str:
    """Ensure *manifest*'s plugin bundle is installed on *sandbox_id*."""
    key = (sandbox_id, manifest.name)
    lock = _locks.setdefault(key, asyncio.Lock())
    async with lock:
        executor = exec_fn or get_adapter(sandbox_id).exec
        digest = _bundle_hash(manifest)
        if await _marker_present(executor, sandbox_id, manifest.name, digest):
            return digest
        await _upload_and_run_setup(
            executor,
            sandbox_id=sandbox_id,
            manifest=manifest,
            digest=digest,
            setup_timeout=setup_timeout,
        )
        return digest


def forget(sandbox_id: str) -> None:
    """Drop process-local install locks for one sandbox id."""
    sandbox_id = str(sandbox_id or "").strip()
    for key in [key for key in _locks if key[0] == sandbox_id]:
        _locks.pop(key, None)


def _bundle_hash(manifest: PluginManifest) -> str:
    hasher = hashlib.sha256()
    for label, path in _hash_inputs(manifest):
        hasher.update(label.encode("utf-8"))
        hasher.update(b"\0")
        hasher.update(path.read_bytes())
        hasher.update(b"\0")
    return hasher.hexdigest()


def _hash_inputs(manifest: PluginManifest) -> list[tuple[str, Path]]:
    """Every regular file under the plugin's source_dir, sorted by relpath.

    Mirrors :func:`_build_tar` so the hash invalidates exactly when the
    bundle does. Skips ``__pycache__`` and dotfiles starting with ``.``
    (e.g. an editor-leftover ``.DS_Store``).
    """
    inputs: list[tuple[str, Path]] = []
    for path in sorted(manifest.source_dir.rglob("*")):
        if not _bundle_includes(path):
            continue
        rel = path.relative_to(manifest.source_dir).as_posix()
        inputs.append((rel, path))
    return inputs


def _bundle_includes(path: Path) -> bool:
    if not path.is_file():
        return False
    parts = path.parts
    if "__pycache__" in parts:
        return False
    if any(part.startswith(".") for part in parts):
        return False
    return True


def _build_tar(manifest: PluginManifest) -> bytes:
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as tar:
        for path in sorted(manifest.source_dir.rglob("*")):
            if not _bundle_includes(path):
                continue
            rel = path.relative_to(manifest.source_dir).as_posix()
            tar.add(
                path,
                arcname=rel,
                filter=_normalize_tarinfo,
            )
    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode="wb", mtime=0) as gz:
        gz.write(raw.getvalue())
    return compressed.getvalue()


def _normalize_tarinfo(info: tarfile.TarInfo) -> tarfile.TarInfo:
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mode = 0o644
    return info


async def _marker_present(
    exec_fn: _RawExecCallable,
    sandbox_id: str,
    plugin_name: str,
    digest: str,
) -> bool:
    marker = plugin_marker_path(plugin_name, digest)
    result = await exec_fn(
        sandbox_id,
        f"test -f {shlex.quote(marker)}",
        timeout=10,
    )
    return getattr(result, "exit_code", 1) == 0


async def _upload_and_run_setup(
    exec_fn: _RawExecCallable,
    *,
    sandbox_id: str,
    manifest: PluginManifest,
    digest: str,
    setup_timeout: int,
) -> None:
    install_dir = plugin_install_dir(manifest.name)
    marker = plugin_marker_path(manifest.name, digest)
    lock_dir = f"{install_dir}.lock"
    staging_dir = f"{install_dir}.staging-{digest[:12]}-{uuid.uuid4().hex[:8]}"
    tar_path = f"{staging_dir}/.bundle.tar.gz"

    bundle = _build_tar(manifest)
    encoded = base64.b64encode(bundle).decode("ascii")

    acquire_lock = await exec_fn(
        sandbox_id,
        (
            f"mkdir -p {shlex.quote(PLUGIN_BUNDLE_REMOTE_ROOT)} && "
            "i=0; "
            f"while ! mkdir {shlex.quote(lock_dir)} 2>/dev/null; do "
            "i=$((i+1)); "
            "if [ \"$i\" -ge 600 ]; then exit 75; fi; "
            "sleep 1; "
            "done"
        ),
        timeout=660,
    )
    _check(acquire_lock, f"plugin install: failed to acquire lock {lock_dir}")
    try:
        if await _marker_present(exec_fn, sandbox_id, manifest.name, digest):
            return
        setup_dir = await exec_fn(
            sandbox_id,
            (
                f"rm -rf {shlex.quote(staging_dir)} && "
                f"mkdir -p {shlex.quote(staging_dir)} && "
                f": > {shlex.quote(tar_path)}"
            ),
            timeout=30,
        )
        _check(setup_dir, f"plugin install: failed to prepare {staging_dir}")

        for offset in range(0, len(encoded), _CHUNK_SIZE):
            chunk = encoded[offset : offset + _CHUNK_SIZE]
            write = await exec_fn(
                sandbox_id,
                (
                    f"printf %s {shlex.quote(chunk)} | base64 -d "
                    f">> {shlex.quote(tar_path)}"
                ),
                timeout=60,
            )
            _check(write, f"plugin install: chunk write failed at offset {offset}")

        extract = await exec_fn(
            sandbox_id,
            (
                f"cd {shlex.quote(staging_dir)} && "
                f"tar -xzf {shlex.quote(tar_path)} && "
                f"rm -f {shlex.quote(tar_path)}"
            ),
            timeout=60,
        )
        _check(extract, "plugin install: bundle extract failed")

        finalize = await exec_fn(
            sandbox_id,
            (
                f"rm -rf {shlex.quote(install_dir)} && "
                f"mv {shlex.quote(staging_dir)} {shlex.quote(install_dir)}"
            ),
            timeout=30,
        )
        _check(finalize, f"plugin install: failed to publish {install_dir}")

        if manifest.setup is not None:
            if not _is_trusted_setup_source(manifest.source_dir):
                raise PluginInstallError(
                    f"plugin {manifest.name!r} setup.sh refused: "
                    f"source_dir {manifest.source_dir} is not under any "
                    f"trusted root; add to EOS_PLUGIN_TRUSTED_SETUP_ROOTS or "
                    f"register_trusted_setup_root() to permit"
                )
            setup_cmd = (
                f"export EOS_PLUGIN_DIR={shlex.quote(install_dir)} && "
                f"chmod +x {shlex.quote(install_dir)}/setup.sh && "
                f"{shlex.quote(install_dir)}/setup.sh"
            )
            setup_run = await exec_fn(
                sandbox_id,
                setup_cmd,
                timeout=setup_timeout,
            )
            if getattr(setup_run, "exit_code", 1) != 0:
                raise PluginInstallError(
                    f"plugin {manifest.name!r} setup.sh failed "
                    f"(exit_code={getattr(setup_run, 'exit_code', 1)}): "
                    f"{getattr(setup_run, 'stderr', '') or getattr(setup_run, 'stdout', '')}"
                )

        write_marker = await exec_fn(
            sandbox_id,
            f"printf %s {shlex.quote(digest)} > {shlex.quote(marker)}",
            timeout=10,
        )
        _check(write_marker, f"plugin install: marker write failed for {marker}")
        logger.info(
            "plugin install: %s sha=%s on %s",
            manifest.name,
            digest[:8],
            sandbox_id,
        )
    finally:
        cleanup = await exec_fn(
            sandbox_id,
            (
                f"rm -rf {shlex.quote(lock_dir)} "
                f"{shlex.quote(staging_dir)}"
            ),
            timeout=30,
        )
        if getattr(cleanup, "exit_code", 1) != 0:
            logger.warning(
                "plugin install: cleanup failed for %s on %s: %s",
                manifest.name,
                sandbox_id,
                getattr(cleanup, "stderr", "") or getattr(cleanup, "stdout", ""),
            )


def _check(result: Any, message: str) -> None:
    if getattr(result, "exit_code", 1) != 0:
        raise PluginInstallError(
            f"{message}: {getattr(result, 'stderr', '') or getattr(result, 'stdout', '')}"
        )
