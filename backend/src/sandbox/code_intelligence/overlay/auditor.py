"""Orchestrator-side overlay shell auditor.

Coordinates one ``svc.cmd`` op per plan §1:

1. Acquire per-sandbox semaphore.
2. Build the live Git snapshot used as the strict-base for tracked writes.
3. Ship the ``overlay_run.py`` runtime bundle into the sandbox, invoke it under
   ``unshare -Urm`` with the user command and snapshot SHA.
4. Read ``$RUN_DIR/diff.ndjson`` from the sandbox.
5. If the script emitted a ``_reject`` meta line → surface via
   ``git_commit_status`` + ``git_conflict_reason`` on the result.
6. Otherwise parse the NDJSON, run OCC over the **gitinclude-route**
   changes via :class:`OverlayCommandCommitter` (first-writer-wins;
   gitignore-route writes were already direct-merged inside the
   namespace with per-file last-writer-wins), and assemble the
   downstream ``SimpleNamespace`` result.
7. Cleanup ``$RUN_DIR``; release semaphore.

Routing terminology: "gitinclude" = every upperdir path ``git
check-ignore`` did *not* flag (OCC route, first-writer-wins).
"gitignore" = every path it did flag (direct-merge, per-file
last-writer-wins, not per-tree atomic). Git index membership is never
consulted; brand-new files absent from the index still go through the
gitinclude route as long as no ``.gitignore`` rule matches them.

Downstream callers (``shell``, ``sandbox.commit.submit_shell_cmd``) read
through a fixed ``SimpleNamespace`` shape:

    result, exit_code, changed_paths, ambient_changed_paths,
    git_commit_status, git_conflict_reason, git_conflict_file

Plus the additive overlay metadata from §4.5 (``gitinclude_changed_paths``,
``gitignore_direct_merged_paths``, ``gitignore_direct_merged_count``,
``mixed_gitinclude_gitignore``, ``mixed_partial_apply``, ``warnings``).
``git_commit_status`` values include ``"committed"`` (OCC succeeded),
``"noop"`` (no gitinclude changes), ``"aborted_version"`` (OCC
strict-base mismatch — first-writer-wins lost the race), and
``"rejected"`` (policy reject from the sandbox-side script).
"""

from __future__ import annotations

import asyncio
import base64
import io
import json
import logging
import posixpath
import shlex
import tarfile
import uuid
from collections.abc import Awaitable, Callable
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from sandbox.daytona_utils import _extract_exit_code, _wrap_bash_command

from sandbox.code_intelligence.overlay.git_snapshot import build_live_snapshot_details
from sandbox.code_intelligence.overlay.command_committer import OverlayCommandCommitter
from sandbox.code_intelligence.overlay.config import (
    overlay_max_concurrent,
    overlay_upper_size_mb,
)
from sandbox.code_intelligence.overlay.types import (
    OverlayChange,
    OverlayDiff,
    OverlayLease,
    OverlayPolicyReject,
    OverlayRunError,
)
from sandbox.code_intelligence.telemetry import record_overlay_op

logger = logging.getLogger(__name__)

_RUN_DIR_PREFIX = "/tmp/eos-shell-overlay"
_PROGRESS_POLL_INTERVAL_SECONDS = 2.0
_PROGRESS_READ_CHUNK_BYTES = 64 * 1024


def _overlay_runtime_bundle_bytes() -> bytes:
    """Return a tar.gz containing the sandbox-side overlay runtime."""
    root = Path(__file__).parent
    runtime_dir = root / "runtime"
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        tar.add(root / "run.py", arcname="overlay_run.py")
        for path in sorted(runtime_dir.rglob("*.py")):
            rel = path.relative_to(runtime_dir).as_posix()
            tar.add(path, arcname=f"overlay_runtime/{rel}")
    return buffer.getvalue()


class OverlayAuditor:
    """Run one command under a fresh ``unshare -Urm`` overlay and commit via OCC."""

    def __init__(
        self,
        *,
        sandbox_id: str,
        workspace_root: str,
        exec_process: Callable[..., Awaitable[Any]],
        write_coordinator: Any,
        max_concurrent: int | None = None,
        upper_size_mb: int | None = None,
    ) -> None:
        self._sandbox_id = sandbox_id
        self._workspace_root = workspace_root.rstrip("/")
        self._exec_process = exec_process
        self._semaphore = asyncio.Semaphore(
            max_concurrent if max_concurrent is not None else overlay_max_concurrent()
        )
        self._upper_size_mb = (
            upper_size_mb if upper_size_mb is not None else overlay_upper_size_mb()
        )
        self._committer = OverlayCommandCommitter(
            write_coordinator, workspace_root=self._workspace_root
        )
        self._script_upload_lock = asyncio.Lock()
        self._script_uploaded = False

    async def execute(
        self,
        sandbox: Any,
        command: str,
        *,
        timeout: int | None = None,
        description: str = "",
        agent_id: str = "",
        run_id: str = "",
        agent_run_id: str = "",
        task_id: str = "",
        stdin: str | None = None,
        attribute_changes: bool = True,
        on_progress_line: Callable[[str], None] | None = None,
    ) -> SimpleNamespace:
        """Run *command* under overlay and return the downstream result shape."""
        del run_id, agent_run_id, task_id  # reserved for ledger enrichment

        async with self._semaphore:
            lease = self._new_lease()
            record_overlay_op(ops_total=1)
            try:
                snapshot = await build_live_snapshot_details(
                    sandbox,
                    self._exec_process,
                    self._workspace_root,
                )
                await self._ensure_script_uploaded(sandbox)
                user_cmd_b64 = base64.b64encode(command.encode("utf-8")).decode("ascii")
                stdin_b64 = (
                    base64.b64encode(stdin.encode("utf-8")).decode("ascii")
                    if stdin is not None
                    else ""
                )
                if on_progress_line is None:
                    stdout_text, script_exit = await self._run_overlay(
                        sandbox,
                        lease=lease,
                        snap=snapshot.snap,
                        user_cmd_b64=user_cmd_b64,
                        stdin_b64=stdin_b64,
                        timeout=timeout,
                    )
                else:
                    stdout_text, script_exit = await self._run_overlay_with_progress(
                        sandbox,
                        lease=lease,
                        snap=snapshot.snap,
                        user_cmd_b64=user_cmd_b64,
                        stdin_b64=stdin_b64,
                        timeout=timeout,
                        on_progress_line=on_progress_line,
                    )
                stdout_text = await self._read_stdout(
                    sandbox, lease, fallback=stdout_text
                )
                diff_or_reject = await self._read_diff(
                    sandbox,
                    lease,
                    overlay_stdout=stdout_text,
                    overlay_exit_code=script_exit,
                )
                if isinstance(diff_or_reject, OverlayPolicyReject):
                    record_overlay_op(
                        ops_rejected=1,
                        dotgit_rejects=(
                            1 if diff_or_reject.reason.endswith("dotgit_writes") else 0
                        ),
                    )
                    return _reject_result(
                        stdout=stdout_text,
                        exit_code=script_exit,
                        reject=diff_or_reject,
                        git_snapshot_timings=(
                            diff_or_reject.snapshot_timings or snapshot.timings
                        ),
                    )
                diff = diff_or_reject
                record_overlay_op(
                    upper_bytes=diff.upper_bytes,
                    upper_files=diff.upper_files,
                    gitinclude_changes=len(diff.gitinclude_changes),
                    gitignore_changes=len(diff.gitignore_paths),
                    direct_merged_bytes=diff.direct_merged_bytes,
                    whiteouts_gitinclude=diff.whiteouts_gitinclude,
                    whiteouts_gitignore_refused=diff.whiteouts_gitignore_refused,
                )
                return await self._commit_and_assemble(
                    stdout=stdout_text,
                    diff=diff,
                    agent_id=agent_id,
                    description=description or "shell overlay",
                    attribute_changes=attribute_changes,
                    git_snapshot_timings=diff.snapshot_timings or snapshot.timings,
                )
            finally:
                try:
                    await self._cleanup_run_dir(sandbox, lease)
                except Exception:
                    logger.debug(
                        "overlay run-dir cleanup failed for %s",
                        lease.run_dir,
                        exc_info=True,
                    )

    # -- internals -----------------------------------------------------------

    def _new_lease(self) -> OverlayLease:
        run_dir = posixpath.join(
            _RUN_DIR_PREFIX, self._sandbox_id, f"run-{uuid.uuid4().hex}"
        )
        return OverlayLease(run_dir=run_dir)

    async def _ensure_script_uploaded(self, sandbox: Any) -> None:
        if self._script_uploaded:
            return
        async with self._script_upload_lock:
            if self._script_uploaded:
                return
            # Read the sandbox-side runtime bundle from the orchestrator
            # package and ship it to /tmp/eos-shell-overlay.
            encoded = base64.b64encode(_overlay_runtime_bundle_bytes()).decode("ascii")
            upload_snippet = (
                "import base64,io,pathlib,sys,tarfile; "
                "root=pathlib.Path(sys.argv[1]); "
                "root.mkdir(parents=True, exist_ok=True); "
                "data=base64.b64decode(sys.argv[2]); "
                "tar=tarfile.open(fileobj=io.BytesIO(data), mode='r:gz'); "
                "\ntry:\n tar.extractall(root, filter='data')"
                "\nexcept TypeError:\n tar.extractall(root)"
            )
            setup_cmd = (
                f"mkdir -p {shlex.quote(_RUN_DIR_PREFIX)} && "
                f"python3 -c {shlex.quote(upload_snippet)} "
                f"{shlex.quote(_RUN_DIR_PREFIX)} {shlex.quote(encoded)}"
            )
            response = await self._exec_process(
                sandbox, _wrap_bash_command(setup_cmd), timeout=60
            )
            _stdout, exit_code = _extract_exit_code(
                str(getattr(response, "result", "") or ""),
                fallback_exit_code=getattr(response, "exit_code", None),
            )
            if exit_code != 0:
                raise OverlayRunError(
                    f"overlay_run.py upload failed: exit_code={exit_code}"
                )
            self._script_uploaded = True

    async def _run_overlay(
        self,
        sandbox: Any,
        *,
        lease: OverlayLease,
        snap: str,
        user_cmd_b64: str,
        stdin_b64: str,
        timeout: int | None,
    ) -> tuple[str, int]:
        script_path = posixpath.join(_RUN_DIR_PREFIX, "overlay_run.py")
        args = [
            "--workspace-root", self._workspace_root,
            "--run-dir", lease.run_dir,
            "--snap", snap,
            "--upper-size-mb", str(self._upper_size_mb),
            "--user-cmd-b64", user_cmd_b64,
        ]
        if stdin_b64:
            args.extend(["--stdin-b64", stdin_b64])
        inner = f"python3 {shlex.quote(script_path)} " + " ".join(
            shlex.quote(a) for a in args
        )
        full = (
            f"mkdir -p {shlex.quote(lease.run_dir)} && "
            f"unshare -Urm bash -c {shlex.quote(inner)}"
        )
        response = await self._exec_process(
            sandbox, _wrap_bash_command(full), timeout=timeout
        )
        stdout_raw = str(getattr(response, "result", "") or "")
        cleaned, exit_code = _extract_exit_code(
            stdout_raw,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return cleaned, exit_code

    async def _run_overlay_with_progress(
        self,
        sandbox: Any,
        *,
        lease: OverlayLease,
        snap: str,
        user_cmd_b64: str,
        stdin_b64: str,
        timeout: int | None,
        on_progress_line: Callable[[str], None],
    ) -> tuple[str, int]:
        task = asyncio.create_task(
            self._run_overlay(
                sandbox,
                lease=lease,
                snap=snap,
                user_cmd_b64=user_cmd_b64,
                stdin_b64=stdin_b64,
                timeout=timeout,
            )
        )
        offset = 0
        partial = ""
        try:
            while not task.done():
                await asyncio.sleep(_PROGRESS_POLL_INTERVAL_SECONDS)
                offset, partial = await self._emit_stdout_progress_delta(
                    sandbox,
                    lease,
                    offset=offset,
                    partial=partial,
                    on_progress_line=on_progress_line,
                )
            stdout_text, exit_code = await task
            offset, partial = await self._emit_stdout_progress_delta(
                sandbox,
                lease,
                offset=offset,
                partial=partial,
                on_progress_line=on_progress_line,
            )
            if partial:
                on_progress_line(partial)
            return stdout_text, exit_code
        except BaseException:
            if not task.done():
                task.cancel()
            raise

    async def _emit_stdout_progress_delta(
        self,
        sandbox: Any,
        lease: OverlayLease,
        *,
        offset: int,
        partial: str,
        on_progress_line: Callable[[str], None],
    ) -> tuple[int, str]:
        try:
            chunk, new_offset = await self._read_stdout_delta(
                sandbox,
                lease,
                offset=offset,
                max_bytes=_PROGRESS_READ_CHUNK_BYTES,
            )
        except Exception:
            logger.debug(
                "overlay stdout progress read failed for %s",
                lease.run_dir,
                exc_info=True,
            )
            return offset, partial
        if not chunk:
            return new_offset, partial
        text = partial + chunk.decode("utf-8", "replace")
        if text.endswith(("\n", "\r")):
            emit_text = text
            partial = ""
        else:
            lines = text.splitlines(keepends=True)
            if lines:
                partial = lines[-1]
                emit_text = "".join(lines[:-1])
            else:
                partial = text
                emit_text = ""
        if emit_text:
            on_progress_line(emit_text)
        return new_offset, partial

    async def _read_stdout(
        self, sandbox: Any, lease: OverlayLease, *, fallback: str
    ) -> str:
        stdout_path = posixpath.join(lease.run_dir, "stdout.bin")
        script = (
            "import base64,pathlib,sys; "
            "sys.stdout.write(base64.b64encode(pathlib.Path(sys.argv[1]).read_bytes()).decode('ascii'))"
        )
        cmd = f"python3 -c {shlex.quote(script)} {shlex.quote(stdout_path)}"
        response = await self._exec_process(
            sandbox, _wrap_bash_command(cmd), timeout=60
        )
        encoded, exit_code = _extract_exit_code(
            str(getattr(response, "result", "") or ""),
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code != 0:
            return fallback
        try:
            return base64.b64decode(encoded.strip()).decode("utf-8", "replace")
        except Exception:
            logger.debug(
                "overlay stdout decode failed for %s", stdout_path, exc_info=True
            )
            return fallback

    async def _read_stdout_delta(
        self,
        sandbox: Any,
        lease: OverlayLease,
        *,
        offset: int,
        max_bytes: int,
    ) -> tuple[bytes, int]:
        stdout_path = posixpath.join(lease.run_dir, "stdout.bin")
        script = (
            "import base64,json,pathlib,sys; "
            "path=pathlib.Path(sys.argv[1]); "
            "offset=max(0,int(sys.argv[2])); "
            "limit=max(1,int(sys.argv[3])); "
            "data=path.read_bytes() if path.exists() else b''; "
            "size=len(data); "
            "start=offset if offset <= size else 0; "
            "start=max(start, size-limit); "
            "chunk=data[start:size]; "
            "print(json.dumps({'start': start, 'size': size, "
            "'chunk': base64.b64encode(chunk).decode('ascii')}))"
        )
        cmd = (
            f"python3 -c {shlex.quote(script)} "
            f"{shlex.quote(stdout_path)} {offset} {max_bytes}"
        )
        response = await self._exec_process(
            sandbox, _wrap_bash_command(cmd), timeout=60
        )
        raw, exit_code = _extract_exit_code(
            str(getattr(response, "result", "") or ""),
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code != 0:
            return b"", offset
        payload = json.loads(raw or "{}")
        size = int(payload.get("size") or 0)
        chunk_b64 = str(payload.get("chunk") or "")
        if not chunk_b64:
            return b"", size
        return base64.b64decode(chunk_b64), size

    async def _read_diff(
        self,
        sandbox: Any,
        lease: OverlayLease,
        *,
        overlay_stdout: str = "",
        overlay_exit_code: int | None = None,
    ) -> OverlayDiff | OverlayPolicyReject:
        diff_path = posixpath.join(lease.run_dir, "diff.ndjson")
        cmd = f"cat {shlex.quote(diff_path)}"
        response = await self._exec_process(
            sandbox, _wrap_bash_command(cmd), timeout=60
        )
        stdout, exit_code = _extract_exit_code(
            str(getattr(response, "result", "") or ""),
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        if exit_code != 0:
            raise OverlayRunError(
                "overlay diff.ndjson missing at "
                f"{diff_path}: cat={stdout[-1000:]!r} "
                f"overlay_exit_code={overlay_exit_code!r} "
                f"overlay_output={overlay_stdout[-2000:]!r}"
            )
        return parse_diff_ndjson(stdout)

    async def _cleanup_run_dir(self, sandbox: Any, lease: OverlayLease) -> None:
        cmd = f"rm -rf {shlex.quote(lease.run_dir)}"
        await self._exec_process(sandbox, _wrap_bash_command(cmd), timeout=60)

    async def _commit_and_assemble(
        self,
        *,
        stdout: str,
        diff: OverlayDiff,
        agent_id: str,
        description: str,
        attribute_changes: bool,
        git_snapshot_timings: dict[str, float] | None = None,
    ) -> SimpleNamespace:
        gitignore_paths = [
            _live_path(self._workspace_root, p) for p in diff.gitignore_paths
        ]
        gitinclude_live_paths = [
            _live_path(self._workspace_root, c.path) for c in diff.gitinclude_changes
        ]

        if not attribute_changes:
            # Treat every change as ambient — do not commit through OCC.
            combined = sorted(set(gitinclude_live_paths + gitignore_paths))
            return _audit_result(
                result_text=stdout,
                exit_code=diff.exit_code,
                gitinclude_committed=[],
                gitignore_merged=gitignore_paths,
                gitignore_merged_count=len(gitignore_paths),
                mixed_gitinclude_gitignore=bool(diff.gitinclude_changes) and bool(
                    diff.gitignore_paths
                ),
                mixed_partial_apply=False,
                ambient=combined,
                git_commit_status=None,
                git_conflict_reason=None,
                git_conflict_file=None,
                warnings=list(diff.warnings),
                git_snapshot_timings=git_snapshot_timings,
            )

        mixed = bool(diff.gitinclude_changes) and bool(diff.gitignore_paths)
        if not diff.gitinclude_changes:
            return _audit_result(
                result_text=stdout,
                exit_code=diff.exit_code,
                gitinclude_committed=[],
                gitignore_merged=gitignore_paths,
                gitignore_merged_count=len(gitignore_paths),
                mixed_gitinclude_gitignore=mixed,
                mixed_partial_apply=False,
                ambient=[],
                git_commit_status="noop",
                git_conflict_reason=None,
                git_conflict_file=None,
                warnings=list(diff.warnings),
                git_snapshot_timings=git_snapshot_timings,
            )

        commit_result = await self._committer.commit(
            diff.gitinclude_changes,
            agent_id=agent_id,
            description=description,
        )
        warnings = list(diff.warnings)
        if commit_result.success:
            return _audit_result(
                result_text=stdout,
                exit_code=diff.exit_code,
                gitinclude_committed=gitinclude_live_paths,
                gitignore_merged=gitignore_paths,
                gitignore_merged_count=len(gitignore_paths),
                mixed_gitinclude_gitignore=mixed,
                mixed_partial_apply=False,
                ambient=[],
                git_commit_status=commit_result.status,
                git_conflict_reason=None,
                git_conflict_file=None,
                warnings=warnings,
                git_snapshot_timings=git_snapshot_timings,
            )

        # Tracked OCC aborted. Gitignored writes were already direct-merged
        # into live — mixed partial-apply contract (plan §4.5).
        partial = mixed
        if partial:
            warnings.append(
                "gitinclude changes aborted by OCC; gitignore runtime changes "
                "were already applied"
            )
            record_overlay_op(
                mixed_partial_apply_ops=1,
                mixed_gitinclude_gitignore_ops=1,
                gitignore_changes_after_aborted_gitinclude=len(gitignore_paths),
            )
        elif mixed:
            record_overlay_op(mixed_gitinclude_gitignore_ops=1)
        return _audit_result(
            result_text=stdout,
            exit_code=diff.exit_code,
            gitinclude_committed=[],
            gitignore_merged=gitignore_paths,
            gitignore_merged_count=len(gitignore_paths),
            mixed_gitinclude_gitignore=mixed,
            mixed_partial_apply=partial,
            ambient=gitinclude_live_paths,
            git_commit_status=commit_result.status,
            git_conflict_reason=commit_result.conflict_reason or None,
            git_conflict_file=commit_result.conflict_file,
            warnings=warnings,
            git_snapshot_timings=git_snapshot_timings,
        )


# ---------------------------------------------------------------------------
# NDJSON parser — decoupled so it can be unit-tested in isolation.
# ---------------------------------------------------------------------------


def parse_diff_ndjson(raw: str) -> OverlayDiff | OverlayPolicyReject:
    """Parse the ``diff.ndjson`` body produced by ``overlay_run.py``.

    Raises :class:`OverlayRunError` on malformed payloads. Returns a
    :class:`OverlayPolicyReject` when the script emitted a ``_reject``
    block, otherwise an :class:`OverlayDiff`.
    """
    lines = [line for line in (raw or "").splitlines() if line.strip()]
    if not lines:
        raise OverlayRunError("empty diff.ndjson payload")

    try:
        first = json.loads(lines[0])
    except json.JSONDecodeError as exc:
        raise OverlayRunError(f"invalid diff.ndjson meta line: {exc}") from exc

    if isinstance(first, dict) and "_reject" in first:
        reject_meta = first["_reject"]
        if not isinstance(reject_meta, dict):
            raise OverlayRunError(f"_reject block must be a dict, got {reject_meta!r}")
        raw_snapshot_timings = reject_meta.get("snapshot_timings") or {}
        raw_run_timings = reject_meta.get("run_timings") or {}
        return OverlayPolicyReject(
            reason=str(reject_meta.get("reason") or ""),
            paths=tuple(str(p) for p in reject_meta.get("paths") or ()),
            snapshot_timings=_parse_timing_dict(raw_snapshot_timings),
            run_timings=_parse_timing_dict(raw_run_timings),
        )

    if not (isinstance(first, dict) and "_meta" in first):
        raise OverlayRunError(
            f"diff.ndjson first line must be _meta or _reject: {first!r}"
        )
    meta = first["_meta"]
    if not isinstance(meta, dict):
        raise OverlayRunError(f"_meta block must be a dict, got {meta!r}")

    changes: list[OverlayChange] = []
    for idx, line in enumerate(lines[1:], start=1):
        try:
            entry = json.loads(line)
        except json.JSONDecodeError as exc:
            raise OverlayRunError(
                f"invalid diff.ndjson entry at line {idx}: {exc}"
            ) from exc
        if not isinstance(entry, dict):
            raise OverlayRunError(
                f"diff.ndjson entry at line {idx} must be a dict: {entry!r}"
            )
        changes.append(
            OverlayChange(
                path=str(entry.get("path") or ""),
                kind=str(entry.get("kind") or "modify"),  # type: ignore[arg-type]
                base_content=str(entry.get("base_content") or ""),
                base_existed=bool(entry.get("base_existed")),
                final_content=(
                    entry["final_content"]
                    if entry.get("final_content") is not None
                    else None
                ),
            )
        )

    gitignore_paths = tuple(str(p) for p in meta.get("gitignore_paths") or ())
    raw_snapshot_timings = meta.get("snapshot_timings") or {}
    raw_run_timings = meta.get("run_timings") or {}
    return OverlayDiff(
        snap=str(meta.get("snap") or ""),
        exit_code=int(meta.get("exit_code") or 0),
        upper_bytes=int(meta.get("upper_bytes") or 0),
        upper_files=int(meta.get("upper_files") or 0),
        gitinclude_changes=tuple(changes),
        gitignore_paths=gitignore_paths,
        gitignore_truncated=bool(meta.get("gitignore_truncated")),
        direct_merged_bytes=int(meta.get("direct_merged_bytes") or 0),
        whiteouts_gitinclude=int(meta.get("whiteouts_gitinclude") or 0),
        whiteouts_gitignore_refused=int(
            meta.get("whiteouts_gitignore_refused") or 0
        ),
        dotgit_rejects=int(meta.get("dotgit_rejects") or 0),
        snapshot_timings=_parse_timing_dict(raw_snapshot_timings),
        run_timings=_parse_timing_dict(raw_run_timings),
        warnings=tuple(str(w) for w in meta.get("warnings") or ()),
    )


def _parse_timing_dict(raw: Any) -> dict[str, float]:
    if not isinstance(raw, dict):
        return {}
    return {
        str(key): round(float(value), 6)
        for key, value in raw.items()
        if isinstance(value, (int, float))
    }


# ---------------------------------------------------------------------------
# Result assembly helpers.
# ---------------------------------------------------------------------------


def _live_path(workspace_root: str, rel: str) -> str:
    rel = rel.replace("\\", "/").lstrip("/")
    return f"{workspace_root}/{rel}"


def _audit_result(
    *,
    result_text: str,
    exit_code: int,
    gitinclude_committed: list[str],
    gitignore_merged: list[str],
    gitignore_merged_count: int,
    mixed_gitinclude_gitignore: bool,
    mixed_partial_apply: bool,
    ambient: list[str],
    git_commit_status: str | None,
    git_conflict_reason: str | None,
    git_conflict_file: str | None,
    warnings: list[str],
    git_snapshot_timings: dict[str, float] | None = None,
) -> SimpleNamespace:
    # Preserve the downstream SimpleNamespace contract (changed_paths,
    # ambient_changed_paths, git_commit_status, git_conflict_reason,
    # git_conflict_file). Everything else is additive metadata.
    return SimpleNamespace(
        result=result_text,
        exit_code=exit_code,
        changed_paths=sorted(gitinclude_committed),
        ambient_changed_paths=sorted(ambient),
        files_written=len(gitinclude_committed),
        git_commit_status=git_commit_status,
        git_conflict_file=git_conflict_file,
        git_conflict_reason=git_conflict_reason,
        gitinclude_changed_paths=sorted(gitinclude_committed),
        gitignore_direct_merged_paths=sorted(gitignore_merged),
        gitignore_direct_merged_count=gitignore_merged_count,
        mixed_gitinclude_gitignore=mixed_gitinclude_gitignore,
        mixed_partial_apply=mixed_partial_apply,
        warnings=list(warnings),
        git_snapshot_timings=dict(git_snapshot_timings or {}),
    )


def _reject_result(
    *,
    stdout: str,
    exit_code: int,
    reject: OverlayPolicyReject,
    git_snapshot_timings: dict[str, float] | None = None,
) -> SimpleNamespace:
    detail = (
        f"{reject.reason}: {','.join(reject.paths)}"
        if reject.paths
        else reject.reason
    )
    return SimpleNamespace(
        result=stdout,
        exit_code=exit_code,
        changed_paths=[],
        ambient_changed_paths=[],
        files_written=0,
        git_commit_status="rejected",
        git_conflict_file=reject.paths[0] if reject.paths else None,
        git_conflict_reason=detail,
        gitinclude_changed_paths=[],
        gitignore_direct_merged_paths=[],
        gitignore_direct_merged_count=0,
        mixed_gitinclude_gitignore=False,
        mixed_partial_apply=False,
        warnings=[detail],
        git_snapshot_timings=dict(git_snapshot_timings or {}),
    )


__all__ = [
    "OverlayAuditor",
    "parse_diff_ndjson",
]
