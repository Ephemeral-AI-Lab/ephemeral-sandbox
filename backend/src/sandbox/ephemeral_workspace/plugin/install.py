"""Idempotent host-side plugin bundle uploader + setup.sh runner.

Mirrors :mod:`sandbox.host.runtime_bundle` but per-plugin: bundles
``plugin.md`` + ``tools/`` + optional ``runtime/`` + ``setup.sh`` from the
host catalog into a gzip tarball, uploads it to
``/eos/daemon/plugins/catalog/<name>/`` on first call, runs
``setup.sh`` once, and writes a ``.installed-<hash>`` marker so subsequent
calls are cheap.

Concurrency: per ``(sandbox_id, plugin_name)`` async lock so concurrent
first-callers share a single upload + setup cycle.
"""

from __future__ import annotations

import asyncio
import gzip
import hashlib
import io
import logging
import os
import shlex
import urllib.request
import tarfile
import uuid
from pathlib import Path
from typing import Any, Protocol

from plugins.core.discovery import DEFAULT_CATALOG_DIR
from plugins.core.manifest import PluginManifest
from sandbox.daemon.paths import BUNDLE_REMOTE_DIR
from sandbox.ephemeral_workspace.plugin.op_registry import _PLUGIN_NAME_RE
from sandbox.host.chunked_upload import RawExecCallable, write_base64_chunks
from sandbox.provider.registry import get_adapter

__all__ = [
    "PLUGIN_BUNDLE_REMOTE_ROOT",
    "PLUGIN_PACKAGE_REMOTE_ROOT",
    "PluginInstallError",
    "ensure_installed",
    "forget_plugin_install_state",
    "plugin_install_dir",
    "plugin_marker_path",
]


logger = logging.getLogger(__name__)

# All plugins land under /eos/daemon/plugins/catalog/<name>/.
# /eos/daemon/ is already on the daemon's sys.path (that's how
# the in-sandbox daemon imports the runtime bundle). plugins/ and
# plugins/catalog/ are implicit namespace packages — no __init__.py is uploaded
# — so ``import plugins.catalog.<name>.runtime.server`` resolves naturally.
PLUGIN_BUNDLE_REMOTE_ROOT = f"{BUNDLE_REMOTE_DIR}/plugins/catalog"
PLUGIN_PACKAGE_REMOTE_ROOT = "/eos/plugin-packages"

# 600s headroom for plugin setup scripts that download runtime binaries or
# install small dependencies over the network while staying inside Daytona's
# exec timeout.
_DEFAULT_SETUP_TIMEOUT = 600

_LSP_NODE_VERSION = "22.13.1"
_LSP_PYRIGHT_VERSION = "1.1.409"


class PutArchiveCallable(Protocol):
    async def __call__(
        self,
        sandbox_id: str,
        *,
        tar_stream: bytes,
        dest_dir: str,
    ) -> None: ...


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

    def __init__(
        self,
        message: str,
        *,
        kind: str = "plugin_install_failed",
        plugin_name: str = "",
        setup_step: str = "",
        command: str = "",
        stderr_excerpt: str = "",
    ) -> None:
        super().__init__(message)
        self.kind = kind
        self.plugin_name = plugin_name
        self.setup_step = setup_step
        self.command = command
        self.stderr_excerpt = stderr_excerpt


_locks: dict[tuple[str, str], asyncio.Lock] = {}
_installed_digests: dict[tuple[str, str], str] = {}


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
    exec_fn: RawExecCallable | None = None,
    put_archive_fn: PutArchiveCallable | None = None,
) -> str:
    """Ensure *manifest*'s plugin bundle is installed on *sandbox_id*."""
    key = (sandbox_id, manifest.name)
    adapter = None if exec_fn is not None else get_adapter(sandbox_id)
    executor = exec_fn or adapter.exec
    put_archive = put_archive_fn
    if put_archive is None and adapter is not None:
        put_archive = getattr(adapter, "put_archive", None)
    bundle_files = _bundle_files(manifest)
    package_files = await _setup_package_files(executor, sandbox_id, manifest)
    digest = _bundle_hash_from_files(bundle_files + package_files)
    if _installed_digests.get(key) == digest:
        return digest

    lock = _locks.setdefault(key, asyncio.Lock())
    async with lock:
        if _installed_digests.get(key) == digest:
            return digest
        if await _marker_present(executor, sandbox_id, manifest.name, digest):
            _installed_digests[key] = digest
            return digest
        await _upload_and_run_setup(
            executor,
            sandbox_id=sandbox_id,
            manifest=manifest,
            digest=digest,
            bundle_files=bundle_files,
            package_files=package_files,
            setup_timeout=setup_timeout,
            put_archive_fn=put_archive,
        )
        _installed_digests[key] = digest
        return digest


def forget_plugin_install_state(sandbox_id: str) -> None:
    """Drop process-local install locks + installed-digest cache for one sandbox id."""
    sandbox_id = str(sandbox_id or "").strip()
    for key in [key for key in _locks if key[0] == sandbox_id]:
        _locks.pop(key, None)
    for key in [key for key in _installed_digests if key[0] == sandbox_id]:
        _installed_digests.pop(key, None)


def _bundle_hash(manifest: PluginManifest) -> str:
    return _bundle_hash_from_files(_bundle_files(manifest))


def _bundle_hash_from_files(bundle_files: list[tuple[str, Path]]) -> str:
    hasher = hashlib.sha256()
    for label, path in bundle_files:
        hasher.update(label.encode("utf-8"))
        hasher.update(b"\0")
        hasher.update(path.read_bytes())
        hasher.update(b"\0")
    return hasher.hexdigest()


def _bundle_files(manifest: PluginManifest) -> list[tuple[str, Path]]:
    """Files included in the plugin bundle, sorted by relative path."""
    source_dir = manifest.source_dir
    files: list[tuple[str, Path]] = []
    for path in sorted(manifest.source_dir.rglob("*")):
        if not _is_bundle_file(path):
            continue
        rel = path.relative_to(source_dir).as_posix()
        files.append((rel, path))
    return files


def _is_bundle_file(path: Path) -> bool:
    if not path.is_file():
        return False
    parts = path.parts
    if "__pycache__" in parts:
        return False
    if any(part.startswith(".") for part in parts):
        return False
    return True


def _build_tar(bundle_files: list[tuple[str, Path]]) -> bytes:
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as tar:
        for rel, path in bundle_files:
            tar.add(
                path,
                arcname=rel,
                filter=_normalize_tarinfo,
            )
    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, mode="wb", mtime=0) as gz:
        gz.write(raw.getvalue())
    return compressed.getvalue()


def _build_plain_tar(entries: list[tuple[str, Path]]) -> bytes:
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w") as tar:
        emitted_dirs: set[str] = set()
        for rel, path in entries:
            clean_rel = rel.strip("/")
            for parent in reversed(Path(clean_rel).parents):
                parent_name = parent.as_posix()
                if parent_name in {"", "."} or parent_name in emitted_dirs:
                    continue
                info = tarfile.TarInfo(parent_name)
                info.type = tarfile.DIRTYPE
                info.mtime = 0
                info.uid = 0
                info.gid = 0
                info.uname = ""
                info.gname = ""
                info.mode = 0o755
                tar.addfile(info)
                emitted_dirs.add(parent_name)
            data = path.read_bytes()
            info = tarfile.TarInfo(clean_rel)
            info.size = len(data)
            info.mtime = 0
            info.uid = 0
            info.gid = 0
            info.uname = ""
            info.gname = ""
            info.mode = 0o755 if os.access(path, os.X_OK) else 0o644
            tar.addfile(info, io.BytesIO(data))
    return raw.getvalue()


def _normalize_tarinfo(info: tarfile.TarInfo) -> tarfile.TarInfo:
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mode = 0o644
    return info


async def _marker_present(
    exec_fn: RawExecCallable,
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
    exec_fn: RawExecCallable,
    *,
    sandbox_id: str,
    manifest: PluginManifest,
    digest: str,
    bundle_files: list[tuple[str, Path]],
    package_files: list[tuple[str, Path]],
    setup_timeout: int,
    put_archive_fn: PutArchiveCallable | None,
) -> None:
    install_dir = plugin_install_dir(manifest.name)
    marker = plugin_marker_path(manifest.name, digest)
    lock_dir = f"{install_dir}.lock"
    staging_dir = f"{install_dir}.staging-{digest[:12]}-{uuid.uuid4().hex[:8]}"

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
        setup_command = (
            f"rm -rf {shlex.quote(staging_dir)} && "
            f"mkdir -p {shlex.quote(PLUGIN_BUNDLE_REMOTE_ROOT)}"
        )
        if put_archive_fn is None:
            setup_command += f" && mkdir -p {shlex.quote(staging_dir)}"
        setup_dir = await exec_fn(sandbox_id, setup_command, timeout=30)
        _check(setup_dir, f"plugin install: failed to prepare {staging_dir}")

        await _upload_entries(
            exec_fn,
            sandbox_id=sandbox_id,
            entries=bundle_files,
            dest_dir=(
                "/eos"
                if put_archive_fn is not None
                else staging_dir
            ),
            archive_prefix=(
                f"daemon/plugins/catalog/{Path(staging_dir).name}"
                if put_archive_fn is not None
                else ""
            ),
            put_archive_fn=put_archive_fn,
            failure_prefix="plugin install: bundle upload",
        )
        uploaded = await exec_fn(
            sandbox_id,
            f"test -f {shlex.quote(staging_dir)}/plugin.md",
            timeout=10,
        )
        if getattr(uploaded, "exit_code", 1) != 0 and put_archive_fn is not None:
            logger.warning(
                "plugin install: put_archive bundle upload did not materialize "
                "for %s on %s; falling back to tar extraction",
                manifest.name,
                sandbox_id,
            )
            fallback_setup = await exec_fn(
                sandbox_id,
                f"mkdir -p {shlex.quote(staging_dir)}",
                timeout=30,
            )
            _check(fallback_setup, "plugin install: fallback staging setup failed")
            await _upload_entries(
                exec_fn,
                sandbox_id=sandbox_id,
                entries=bundle_files,
                dest_dir=staging_dir,
                archive_prefix="",
                put_archive_fn=None,
                failure_prefix="plugin install: fallback bundle upload",
            )
            uploaded = await exec_fn(
                sandbox_id,
                f"test -f {shlex.quote(staging_dir)}/plugin.md",
                timeout=10,
            )
        _check(uploaded, "plugin install: bundle upload missing plugin.md")

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
            package_dir = f"{PLUGIN_PACKAGE_REMOTE_ROOT}/{manifest.name}"
            if package_files:
                prepare_package_root = await exec_fn(
                    sandbox_id,
                    f"mkdir -p {shlex.quote(PLUGIN_PACKAGE_REMOTE_ROOT)}",
                    timeout=30,
                )
                _check(
                    prepare_package_root,
                    "plugin install: package root setup failed",
                )
                await _upload_entries(
                    exec_fn,
                    sandbox_id=sandbox_id,
                    entries=package_files,
                    dest_dir="/eos" if put_archive_fn is not None else PLUGIN_PACKAGE_REMOTE_ROOT,
                    archive_prefix="plugin-packages" if put_archive_fn is not None else "",
                    put_archive_fn=put_archive_fn,
                    failure_prefix="plugin install: package upload",
                )
                package_uploaded = await exec_fn(
                    sandbox_id,
                    (
                        f"test -s {shlex.quote(package_dir)}/node.tar.xz && "
                        f"test -s {shlex.quote(package_dir)}/pyright.tgz"
                    ),
                    timeout=10,
                )
                if (
                    getattr(package_uploaded, "exit_code", 1) != 0
                    and put_archive_fn is not None
                ):
                    logger.warning(
                        "plugin install: put_archive package upload did not "
                        "materialize for %s on %s; falling back to tar extraction",
                        manifest.name,
                        sandbox_id,
                    )
                    fallback_package_root = await exec_fn(
                        sandbox_id,
                        f"mkdir -p {shlex.quote(PLUGIN_PACKAGE_REMOTE_ROOT)}",
                        timeout=30,
                    )
                    _check(
                        fallback_package_root,
                        "plugin install: fallback package root setup failed",
                    )
                    await _upload_entries(
                        exec_fn,
                        sandbox_id=sandbox_id,
                        entries=package_files,
                        dest_dir=PLUGIN_PACKAGE_REMOTE_ROOT,
                        archive_prefix="",
                        put_archive_fn=None,
                        failure_prefix="plugin install: fallback package upload",
                    )
                    package_uploaded = await exec_fn(
                        sandbox_id,
                        (
                            f"test -s {shlex.quote(package_dir)}/node.tar.xz && "
                            f"test -s {shlex.quote(package_dir)}/pyright.tgz"
                        ),
                        timeout=10,
                    )
                _check(
                    package_uploaded,
                    "plugin install: package upload missing setup artifacts",
                )
            if not _is_trusted_setup_source(manifest.source_dir):
                raise PluginInstallError(
                    f"plugin {manifest.name!r} setup.sh refused: "
                    f"source_dir {manifest.source_dir} is not under any "
                    "trusted root; add it to EOS_PLUGIN_TRUSTED_SETUP_ROOTS "
                    "to permit setup execution",
                    kind="plugin_setup_refused",
                    plugin_name=manifest.name,
                    setup_step="setup.sh",
                )
            setup_cmd = (
                f"export EOS_PLUGIN_DIR={shlex.quote(install_dir)} && "
                f"export EOS_PLUGIN_PACKAGE_DIR={shlex.quote(package_dir)} && "
                f"chmod +x {shlex.quote(install_dir)}/setup.sh && "
                f"{shlex.quote(install_dir)}/setup.sh"
            )
            setup_run = await exec_fn(
                sandbox_id,
                setup_cmd,
                timeout=setup_timeout,
            )
            if getattr(setup_run, "exit_code", 1) != 0:
                setup_stderr = str(
                    getattr(setup_run, "stderr", "")
                    or getattr(setup_run, "stdout", "")
                    or ""
                )
                raise PluginInstallError(
                    f"plugin {manifest.name!r} setup.sh failed "
                    f"(exit_code={getattr(setup_run, 'exit_code', 1)}): "
                    f"{setup_stderr}",
                    kind=_classify_setup_failure(setup_stderr),
                    plugin_name=manifest.name,
                    setup_step="setup.sh",
                    command=setup_cmd,
                    stderr_excerpt=setup_stderr[:500],
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


async def _upload_entries(
    exec_fn: RawExecCallable,
    *,
    sandbox_id: str,
    entries: list[tuple[str, Path]],
    dest_dir: str,
    archive_prefix: str,
    put_archive_fn: PutArchiveCallable | None,
    failure_prefix: str,
) -> None:
    if put_archive_fn is not None:
        await put_archive_fn(
            sandbox_id,
            tar_stream=_build_plain_tar(_prefixed_entries(entries, archive_prefix)),
            dest_dir=dest_dir,
        )
        return

    tar_path = f"{dest_dir}/.upload-{uuid.uuid4().hex}.tar.gz"
    bundle = _build_tar(entries)
    setup_tar = await exec_fn(
        sandbox_id,
        f": > {shlex.quote(tar_path)}",
        timeout=30,
    )
    _check(setup_tar, f"{failure_prefix}: failed to prepare tarball")
    await write_base64_chunks(
        exec_fn,
        sandbox_id,
        content=bundle,
        remote_path=tar_path,
        check_result=_check,
        failure_message=lambda offset: (
            f"{failure_prefix}: chunk write failed at offset {offset}"
        ),
    )
    extract = await exec_fn(
        sandbox_id,
        (
            f"cd {shlex.quote(dest_dir)} && "
            f"tar -xzf {shlex.quote(tar_path)} && "
            f"rm -f {shlex.quote(tar_path)}"
        ),
        timeout=60,
    )
    _check(extract, f"{failure_prefix}: extract failed")


def _prefixed_entries(
    entries: list[tuple[str, Path]],
    prefix: str,
) -> list[tuple[str, Path]]:
    clean = prefix.strip("/")
    if not clean:
        return entries
    return [(f"{clean}/{rel}", path) for rel, path in entries]


async def _setup_package_files(
    exec_fn: RawExecCallable,
    sandbox_id: str,
    manifest: PluginManifest,
) -> list[tuple[str, Path]]:
    if manifest.setup is None or manifest.name != "lsp":
        return []
    arch = _node_arch(
        await _exec_stdout(exec_fn, sandbox_id, "uname -m", timeout=15)
    )
    package_dir = _plugin_package_cache_dir(manifest.name)
    package_dir.mkdir(parents=True, exist_ok=True)
    node_archive = package_dir / f"node-v{_LSP_NODE_VERSION}-linux-{arch}.tar.xz"
    pyright_archive = package_dir / f"pyright-{_LSP_PYRIGHT_VERSION}.tgz"
    _download_file(
        _node_urls(arch),
        node_archive,
        label=f"Node {_LSP_NODE_VERSION} linux-{arch}",
    )
    _download_file(
        _pyright_urls(),
        pyright_archive,
        label=f"pyright {_LSP_PYRIGHT_VERSION}",
    )
    manifest_file = package_dir / "PACKAGE_MANIFEST.txt"
    manifest_file.write_text(
        "\n".join(
            (
                f"plugin={manifest.name}",
                f"node_version={_LSP_NODE_VERSION}",
                f"node_arch={arch}",
                f"pyright_version={_LSP_PYRIGHT_VERSION}",
            )
        )
        + "\n",
        encoding="utf-8",
    )
    return [
        (f"{manifest.name}/node.tar.xz", node_archive),
        (f"{manifest.name}/pyright.tgz", pyright_archive),
        (f"{manifest.name}/PACKAGE_MANIFEST.txt", manifest_file),
    ]


async def _exec_stdout(
    exec_fn: RawExecCallable,
    sandbox_id: str,
    command: str,
    *,
    timeout: int,
) -> str:
    result = await exec_fn(sandbox_id, command, timeout=timeout)
    _check(result, f"plugin install: command failed: {command}")
    return str(getattr(result, "stdout", "") or "").strip()


def _plugin_package_cache_dir(plugin_name: str) -> Path:
    root = os.environ.get("EOS_PLUGIN_PACKAGE_CACHE")
    base = Path(root).expanduser() if root else Path.home() / ".cache" / "ephemeralos" / "plugin-packages"
    return base / plugin_name


def _node_arch(machine: str) -> str:
    normalized = machine.strip().lower()
    if normalized == "x86_64":
        return "x64"
    if normalized in {"aarch64", "arm64"}:
        return "arm64"
    raise PluginInstallError(
        f"unsupported Node plugin package architecture: {machine!r}",
        kind="plugin_package_unsupported_arch",
        plugin_name="lsp",
        setup_step="host_package",
    )


def _node_urls(arch: str) -> list[str]:
    archive = f"node-v{_LSP_NODE_VERSION}-linux-{arch}.tar.xz"
    raw = os.environ.get("EOS_NODE_DOWNLOAD_URLS", "").strip()
    if raw:
        return [url for url in raw.split() if url.strip()]
    return [
        f"https://registry.npmmirror.com/-/binary/node/v{_LSP_NODE_VERSION}/{archive}",
        f"https://nodejs.org/dist/v{_LSP_NODE_VERSION}/{archive}",
    ]


def _pyright_urls() -> list[str]:
    raw = os.environ.get("EOS_PYRIGHT_TARBALL_URLS", "").strip()
    if raw:
        return [url for url in raw.split() if url.strip()]
    return [
        f"https://registry.npmjs.org/pyright/-/pyright-{_LSP_PYRIGHT_VERSION}.tgz",
        f"https://registry.npmmirror.com/pyright/-/pyright-{_LSP_PYRIGHT_VERSION}.tgz",
    ]


def _download_file(urls: list[str], dest: Path, *, label: str) -> None:
    if dest.is_file() and dest.stat().st_size > 0:
        return
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + f".{uuid.uuid4().hex}.tmp")
    errors: list[str] = []
    for url in urls:
        try:
            urllib.request.urlretrieve(url, tmp)
            if tmp.stat().st_size <= 0:
                raise OSError(f"empty download from {url}")
            tmp.replace(dest)
            return
        except Exception as exc:  # noqa: BLE001 - report every attempted mirror.
            errors.append(f"{url}: {type(exc).__name__}: {exc}")
            try:
                tmp.unlink()
            except FileNotFoundError:
                pass
    raise PluginInstallError(
        f"failed to download {label} on host: {'; '.join(errors)}",
        kind="plugin_package_download_failed",
        plugin_name="lsp",
        setup_step="host_package",
        stderr_excerpt="; ".join(errors)[:500],
    )


def _check(result: Any, message: str) -> None:
    if getattr(result, "exit_code", 1) != 0:
        raise PluginInstallError(
            f"{message}: {getattr(result, 'stderr', '') or getattr(result, 'stdout', '')}"
        )


def _classify_setup_failure(stderr: str) -> str:
    lowered = stderr.lower()
    network_needles = (
        "network",
        "could not resolve",
        "connection refused",
        "connection timed out",
        "temporary failure",
        "ssl_connect",
        "curl:",
        "npm err!",
        "registry.npmjs.org",
        "nodejs.org",
    )
    if any(needle in lowered for needle in network_needles):
        return "plugin_setup_network_failure"
    return "plugin_setup_failed"
