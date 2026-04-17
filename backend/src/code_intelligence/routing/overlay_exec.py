"""Per-run overlayfs exec primitive.

Runs one user command inside a fresh ``unshare -Urm`` user+mount namespace
with a size-capped tmpfs backing an overlay whose lowerdir is a frozen
snapshot of the repo. The user command sees the repo at its normal
``repo_root`` path (achieved via ``mount --bind merged repo_root`` inside
the namespace) but every write it performs lands in the overlay upperdir.

The upperdir is packaged into a tarball on the container filesystem
**before** the namespace exits (tmpfs dies with the namespace). The
tarball is the input to :class:`UpperdirWalker`.

The remote exec always returns exit 0; the user command's real exit code
is emitted inside sentinel-framed output so transport failures are
distinguishable from user-command failures (same design rule as
``ProcessAuditor``).

Capability requirement: ``unshare -Urm`` must work and overlay with
``userxattr`` option and tmpfs-backed upperdir must mount successfully.
See :mod:`overlay_probe` for detection.
"""

from __future__ import annotations

import base64
import binascii
import logging
import re
import shlex
import uuid
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger(__name__)

_SENTINEL_PREFIX = "__OVERLAYAUDIT_"
_SECTIONS = ("EXEC", "EXIT", "TAR", "MOUNT_ERR")
_DEFAULT_TMPFS_SIZE = "2g"


def _sentinel(run_id: str, section: str, side: str) -> str:
    return f"{_SENTINEL_PREFIX}{run_id}_{section}_{side}__"


class OverlayExecError(RuntimeError):
    """Raised when the overlay exec transport fails or output is unparseable."""


class OverlayMountError(RuntimeError):
    """Raised when the overlay / tmpfs / bind mount step fails.

    Callers should interpret this as "the overlay is unavailable on this
    sandbox" and fall back to a simpler auditor.
    """


@dataclass(frozen=True)
class OverlayRunResult:
    """Outcome of one audited overlay execution."""

    run_id: str
    exit_code: int
    stdout: str
    """User command's captured stdout+stderr (merged, base64-safe-decoded)."""
    audit_tar_path: str
    """Absolute path on the container filesystem to the upperdir tarball."""
    run_dir: str
    """Scratch directory on container filesystem; caller must clean up."""


def _build_inner_script(
    *,
    user_command: str,
    lowerdir: str,
    repo_root: str,
    run_dir: str,
    tmpfs_size: str,
) -> str:
    """Return the inner script executed inside ``unshare -Urm``.

    Exposed for tests.
    """
    quoted_cmd = shlex.quote(user_command)
    quoted_lower = shlex.quote(lowerdir)
    quoted_repo = shlex.quote(repo_root)
    quoted_run = shlex.quote(run_dir)
    quoted_size = shlex.quote(tmpfs_size)
    return (
        "set +e\n"
        f"mkdir -p {quoted_run}/ns || exit 90\n"
        f"mount -t tmpfs -o size={quoted_size} tmpfs {quoted_run}/ns 2> {quoted_run}/mount_err\n"
        f"[ $? -eq 0 ] || {{ cat {quoted_run}/mount_err; exit 91; }}\n"
        f"mkdir -p {quoted_run}/ns/upper {quoted_run}/ns/work {quoted_run}/ns/merged\n"
        f"mount -t overlay overlay "
        f"-o lowerdir={quoted_lower},upperdir={quoted_run}/ns/upper,"
        f"workdir={quoted_run}/ns/work,userxattr "
        f"{quoted_run}/ns/merged 2>> {quoted_run}/mount_err\n"
        f"[ $? -eq 0 ] || {{ cat {quoted_run}/mount_err; exit 92; }}\n"
        f"mkdir -p {quoted_repo}\n"
        f"mount --bind {quoted_run}/ns/merged {quoted_repo} 2>> {quoted_run}/mount_err\n"
        f"[ $? -eq 0 ] || {{ cat {quoted_run}/mount_err; exit 93; }}\n"
        f"cd {quoted_repo}\n"
        f"bash -c {quoted_cmd} > {quoted_run}/exec.out 2>&1\n"
        f"echo $? > {quoted_run}/exit\n"
        # --xattrs preserves opaque-dir markers (user.overlay.opaque=y).
        # --acls omitted (not used by overlayfs in userxattr mode).
        f"tar --numeric-owner --xattrs --xattrs-include='user.overlay.*' "
        f"-cf {quoted_run}/audit.tar -C {quoted_run}/ns/upper . "
        f"2>> {quoted_run}/mount_err\n"
        f"echo $? > {quoted_run}/tar_rc\n"
        f"umount {quoted_run}/ns/merged 2>/dev/null\n"
        f"umount {quoted_run}/ns 2>/dev/null\n"
        "exit 0\n"
    )


def _build_overlay_bash(
    *,
    user_command: str,
    lowerdir: str,
    repo_root: str,
    run_dir: str,
    run_id: str,
    tmpfs_size: str,
) -> str:
    """Construct the full ``bash -c`` wrapper for one overlay run.

    Layout inside the remote invocation:

    * ``$run_dir``               — container-fs scratch; persists after
      the namespace exits (holds the tarball and exit-code file).
    * ``$run_dir/ns``            — tmpfs mount inside the namespace;
      backs ``upper/``, ``work/``, and ``merged/``. Destroyed at ns exit.
    * ``$run_dir/audit.tar``     — tarball of ``upper/`` produced before
      the ns exits.
    * ``$run_dir/exit``          — user-command exit code.

    The script is fire-and-forget from the transport's perspective: the
    outer exec always returns 0; real user exit code is read from the
    EXIT sentinel frame.
    """
    quoted_run = shlex.quote(run_dir)

    def pair(section: str) -> tuple[str, str]:
        return _sentinel(run_id, section, "OPEN"), _sentinel(run_id, section, "CLOSE")

    exec_open, exec_close = pair("EXEC")
    exit_open, exit_close = pair("EXIT")
    tar_open, tar_close = pair("TAR")
    merr_open, merr_close = pair("MOUNT_ERR")

    inner = _build_inner_script(
        user_command=user_command,
        lowerdir=lowerdir,
        repo_root=repo_root,
        run_dir=run_dir,
        tmpfs_size=tmpfs_size,
    )
    inner_b64 = base64.b64encode(inner.encode("utf-8")).decode("ascii")

    # Outer script: sets up run_dir on container fs, runs inner inside unshare,
    # then emits all the framed sections for the parent to parse.
    outer = (
        "set -u\n"
        f"mkdir -p {quoted_run} || exit 80\n"
        f"INNER_SCRIPT={quoted_run}/inner.sh\n"
        f"printf '%s' {shlex.quote(inner_b64)} | base64 -d > \"$INNER_SCRIPT\"\n"
        f"chmod +x \"$INNER_SCRIPT\"\n"
        f"unshare -Urm bash \"$INNER_SCRIPT\"\n"
        f"OUTER_RC=$?\n"
        # EXEC: user command stdout+stderr, base64 for safety.
        f"printf '%s\\n' {shlex.quote(exec_open)}\n"
        f"if [ -f {quoted_run}/exec.out ]; then "
        f"base64 < {quoted_run}/exec.out | tr -d '\\n'; fi\n"
        f"printf '\\n%s\\n' {shlex.quote(exec_close)}\n"
        # EXIT: user command exit code; -1 if missing (mount error before exec).
        f"printf '%s\\n' {shlex.quote(exit_open)}\n"
        f"if [ -f {quoted_run}/exit ]; then cat {quoted_run}/exit; else echo -1; fi\n"
        f"printf '\\n%s\\n' {shlex.quote(exit_close)}\n"
        # TAR: tarball path + tar_rc, both needed to decide whether walker runs.
        f"printf '%s\\n' {shlex.quote(tar_open)}\n"
        f"printf '%s|%s' {quoted_run}/audit.tar \"$(cat {quoted_run}/tar_rc 2>/dev/null || echo -1)\"\n"
        f"printf '\\n%s\\n' {shlex.quote(tar_close)}\n"
        # MOUNT_ERR: if outer rc indicates a mount failure, surface the err log.
        f"printf '%s\\n' {shlex.quote(merr_open)}\n"
        f"if [ \"$OUTER_RC\" -ne 0 ] && [ -f {quoted_run}/mount_err ]; then "
        f"printf 'rc=%s\\n' \"$OUTER_RC\"; cat {quoted_run}/mount_err; fi\n"
        f"printf '\\n%s\\n' {shlex.quote(merr_close)}\n"
        "exit 0\n"
    )

    return f"env -u LC_ALL bash -o pipefail -c {shlex.quote(outer)}"


def _extract(raw: str, run_id: str, section: str) -> str:
    open_tok = re.escape(_sentinel(run_id, section, "OPEN"))
    close_tok = re.escape(_sentinel(run_id, section, "CLOSE"))
    match = re.search(
        rf"{open_tok}\n(.*?)\n{close_tok}",
        raw,
        flags=re.DOTALL,
    )
    if match is None:
        raise OverlayExecError(f"missing section {section} in overlay exec output")
    return match.group(1)


def _decode_b64(payload: str, section: str) -> bytes:
    stripped = payload.strip()
    if not stripped:
        return b""
    try:
        return base64.b64decode(stripped, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise OverlayExecError(f"section {section} base64 decode failed: {exc}") from exc


class OverlayExec:
    """Async facade over one overlay-audited sandbox exec.

    Parameters
    ----------
    exec_process:
        Awaitable that runs one command on the sandbox and returns an
        object exposing ``.result`` (stdout) and optionally
        ``.exit_code``. Same contract as
        :attr:`ProcessAuditor._exec_process`.
    tmpfs_size:
        Upperdir tmpfs size cap (e.g. ``"2g"``). Writes exceeding this
        get ENOSPC inside the user command rather than risking container
        OOM.
    """

    def __init__(
        self,
        *,
        exec_process: Callable[..., Awaitable[Any]],
        tmpfs_size: str = _DEFAULT_TMPFS_SIZE,
    ) -> None:
        self._exec_process = exec_process
        self._tmpfs_size = tmpfs_size

    async def execute(
        self,
        sandbox: Any,
        user_command: str,
        *,
        lowerdir: str,
        repo_root: str,
        run_dir: str | None = None,
        timeout: int | None = None,
    ) -> OverlayRunResult:
        """Run ``user_command`` under a fresh overlay. One remote exec.

        Raises
        ------
        OverlayMountError
            The unshare / tmpfs / overlay / bind pipeline failed.
            Treat as "overlay auditing unavailable" and fall back.
        OverlayExecError
            The transport succeeded but its output is unparseable.
        """
        run_id = uuid.uuid4().hex
        scratch = run_dir or f"/tmp/overlay-{run_id}"
        cmd = _build_overlay_bash(
            user_command=user_command,
            lowerdir=lowerdir,
            repo_root=repo_root,
            run_dir=scratch,
            run_id=run_id,
            tmpfs_size=self._tmpfs_size,
        )
        effective_timeout = (timeout + 60) if timeout is not None else None
        response = await self._exec_process(sandbox, cmd, timeout=effective_timeout)
        raw = str(getattr(response, "result", "") or "")

        mount_err = _extract(raw, run_id, "MOUNT_ERR").strip()
        if mount_err:
            raise OverlayMountError(mount_err)

        exec_b64 = _extract(raw, run_id, "EXEC")
        stdout = _decode_b64(exec_b64, "EXEC").decode("utf-8", errors="replace")

        exit_raw = _extract(raw, run_id, "EXIT").strip()
        try:
            exit_code = int(exit_raw)
        except ValueError as exc:
            raise OverlayExecError(
                f"EXIT section not an integer: {exit_raw!r}"
            ) from exc

        tar_raw = _extract(raw, run_id, "TAR").strip()
        try:
            tar_path, tar_rc_str = tar_raw.rsplit("|", 1)
        except ValueError as exc:
            raise OverlayExecError(f"TAR section malformed: {tar_raw!r}") from exc
        try:
            tar_rc = int(tar_rc_str)
        except ValueError as exc:
            raise OverlayExecError(
                f"TAR rc not an integer: {tar_rc_str!r}"
            ) from exc
        if tar_rc != 0:
            raise OverlayMountError(
                f"upperdir tar failed with rc={tar_rc}; the overlay mount likely "
                "did not complete. Treat as mount failure for fallback purposes."
            )

        return OverlayRunResult(
            run_id=run_id,
            exit_code=exit_code,
            stdout=stdout,
            audit_tar_path=tar_path,
            run_dir=scratch,
        )


__all__ = [
    "OverlayExec",
    "OverlayExecError",
    "OverlayMountError",
    "OverlayRunResult",
]
