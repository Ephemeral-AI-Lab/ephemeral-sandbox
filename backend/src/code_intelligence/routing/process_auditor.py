"""Process-level workspace mutation auditing."""

from __future__ import annotations

import base64
import binascii
import json
import logging
import re
import shlex
import uuid
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from types import SimpleNamespace
from typing import Any

logger = logging.getLogger(__name__)

_PROCESS_AUDIT_SNAPSHOT_SCRIPT = r"""
import hashlib
import json
import pathlib
import subprocess
import sys

root = pathlib.Path(sys.argv[1]).resolve()


def _split_z(raw):
    return [item for item in raw.split("\0") if item]


def _run_git(args):
    proc = subprocess.run(
        ["git", "-C", str(root), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    if proc.returncode != 0:
        return []
    return _split_z(proc.stdout)


def _hash_bytes(payload):
    return hashlib.sha256(payload).hexdigest()[:16]


def _head_hash(rel_path):
    proc = subprocess.run(
        ["git", "-C", str(root), "show", f"HEAD:{rel_path}"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if proc.returncode != 0:
        return ""
    return _hash_bytes(proc.stdout)


def _inside_root(path):
    try:
        path.relative_to(root)
    except ValueError:
        return False
    return True


git_probe = subprocess.run(
    ["git", "-C", str(root), "rev-parse", "--is-inside-work-tree"],
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    text=True,
)
git_ok = git_probe.returncode == 0 and git_probe.stdout.strip() == "true"
paths = set()

if git_ok:
    for args in (
        ["diff", "--name-only", "-z"],
        ["diff", "--cached", "--name-only", "-z"],
        ["ls-files", "--others", "--exclude-standard", "-z"],
        ["ls-files", "--deleted", "-z"],
    ):
        paths.update(_run_git(args))
else:
    for candidate in root.rglob("*"):
        if ".git" in candidate.parts:
            continue
        if candidate.is_file():
            paths.add(str(candidate.relative_to(root)))

files = {}
for rel in sorted(paths):
    if not rel:
        continue
    path = (root / rel).resolve()
    if not _inside_root(path):
        continue
    exists = path.exists() and path.is_file()
    digest = ""
    if exists:
        try:
            digest = _hash_bytes(path.read_bytes())
        except OSError:
            digest = ""
    files[str(path)] = {
        "rel": rel,
        "exists": exists,
        "hash": digest,
        "head_hash": _head_hash(rel) if git_ok else "",
    }

print(json.dumps({"ok": True, "files": files}))
"""


# Pre-encode the snapshot script once at import time so the outer bash wrapper
# can embed it inline without re-encoding per call.
_SNAPSHOT_SCRIPT_B64 = base64.b64encode(
    _PROCESS_AUDIT_SNAPSHOT_SCRIPT.encode("utf-8")
).decode("ascii")


_PROCESS_AUDIT_SENTINEL_PREFIX = "__CIAUDIT_"
_SECTIONS = ("BEFORE", "EXEC", "EXIT", "AFTER")


def _sentinel(run_id: str, section: str, side: str) -> str:
    return f"{_PROCESS_AUDIT_SENTINEL_PREFIX}{run_id}_{section}_{side}__"


class _ProcessAuditFrameError(RuntimeError):
    """Raised when the combined exec output cannot be parsed."""


@dataclass(frozen=True)
class _CombinedExecOutput:
    before: dict[str, dict[str, Any]]
    after: dict[str, dict[str, Any]]
    exec_stdout: str
    exec_exit_code: int


def _build_process_audit_combined_bash(
    command: str,
    *,
    workspace_root: str,
    run_id: str,
) -> str:
    """Build one ``env -u LC_ALL bash -o pipefail -c <script>`` invocation.

    Pipeline inside the outer bash script:

      1. ``mktemp -d`` → ``$_OUT_DIR``; trap removes it on exit.
      2. Decode ``_PROCESS_AUDIT_SNAPSHOT_SCRIPT`` (base64) into
         ``$_OUT_DIR/snapshot.py``.
      3. ``python3 snapshot.py <workspace>`` → base64-encode → emit between
         BEFORE sentinels.
      4. Run the user ``command`` with merged stdout+stderr captured to
         ``$_OUT_DIR/exec.out`` and its exit code to ``$_OUT_DIR/exec.code``
         (``set +e`` so non-zero does not abort the outer pipeline).
      5. Base64-encode ``exec.out`` → emit between EXEC sentinels.
      6. ``cat exec.code`` → emit between EXIT sentinels (plain ASCII int).
      7. ``python3 snapshot.py <workspace>`` again → base64 → emit between
         AFTER sentinels.
      8. Outer script always ``exit 0`` so exec failures come through the
         EXIT section rather than the transport exit code.

    The user ``command`` is treated as an opaque shell string -- in production
    it has already been wrapped by ``_wrap_bash_command``. We hand it to an
    inner ``bash -c`` via ``shlex.quote`` so no double-wrapping is required
    and the EXEC payload naturally ends with ``__CODEX_EXIT_CODE__=N`` which
    downstream ``_extract_exit_code`` consumes.

    ``base64 | tr -d '\\n'`` is used instead of ``-w0`` for macOS/BSD
    portability (BSD ``base64`` has no ``-w`` flag).
    """
    quoted_workspace = shlex.quote(workspace_root)
    quoted_command = shlex.quote(command)
    quoted_script_b64 = shlex.quote(_SNAPSHOT_SCRIPT_B64)

    def _pair(section: str) -> tuple[str, str]:
        return _sentinel(run_id, section, "OPEN"), _sentinel(run_id, section, "CLOSE")

    before_open, before_close = _pair("BEFORE")
    exec_open, exec_close = _pair("EXEC")
    exit_open, exit_close = _pair("EXIT")
    after_open, after_close = _pair("AFTER")

    outer_script = (
        "set -u\n"
        '_OUT_DIR="$(mktemp -d)" || exit 1\n'
        "trap 'rm -rf \"$_OUT_DIR\"' EXIT\n"
        f"printf '%s\\n' {quoted_script_b64} | base64 -d > \"$_OUT_DIR/snapshot.py\"\n"
        f"printf '\\n%s\\n' {shlex.quote(before_open)}\n"
        f"python3 \"$_OUT_DIR/snapshot.py\" {quoted_workspace} 2>/dev/null | base64 | tr -d '\\n'\n"
        f"printf '\\n%s\\n' {shlex.quote(before_close)}\n"
        "set +e\n"
        f"bash -c {quoted_command} > \"$_OUT_DIR/exec.out\" 2>&1\n"
        'printf "%s" "$?" > "$_OUT_DIR/exec.code"\n'
        "set -u\n"
        f"printf '%s\\n' {shlex.quote(exec_open)}\n"
        "base64 < \"$_OUT_DIR/exec.out\" | tr -d '\\n'\n"
        f"printf '\\n%s\\n' {shlex.quote(exec_close)}\n"
        f"printf '%s\\n' {shlex.quote(exit_open)}\n"
        'cat "$_OUT_DIR/exec.code"\n'
        f"printf '\\n%s\\n' {shlex.quote(exit_close)}\n"
        f"printf '%s\\n' {shlex.quote(after_open)}\n"
        f"python3 \"$_OUT_DIR/snapshot.py\" {quoted_workspace} 2>/dev/null | base64 | tr -d '\\n'\n"
        f"printf '\\n%s\\n' {shlex.quote(after_close)}\n"
        "exit 0\n"
    )

    return f"env -u LC_ALL bash -o pipefail -c {shlex.quote(outer_script)}"


def _extract_section(raw: str, run_id: str, section: str) -> str:
    open_token = re.escape(_sentinel(run_id, section, "OPEN"))
    close_token = re.escape(_sentinel(run_id, section, "CLOSE"))
    pattern = re.compile(
        rf"{open_token}\n(.*?)\n{close_token}",
        flags=re.DOTALL,
    )
    match = pattern.search(raw)
    if match is None:
        raise _ProcessAuditFrameError(
            f"missing section {section} for run {run_id}"
        )
    return match.group(1)


def _decode_b64_payload(raw: str, section: str) -> bytes:
    stripped = raw.strip()
    if not stripped:
        return b""
    try:
        return base64.b64decode(stripped, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise _ProcessAuditFrameError(
            f"section {section} base64 decode failed: {exc}"
        ) from exc


def _decode_snapshot(payload: bytes, section: str) -> dict[str, dict[str, Any]]:
    text = payload.decode("utf-8", errors="replace").strip()
    if not text:
        logger.debug("process audit %s snapshot payload was empty", section)
        return {}
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError as exc:
        raise _ProcessAuditFrameError(
            f"section {section} JSON decode failed: {exc}"
        ) from exc
    if not isinstance(parsed, dict):
        raise _ProcessAuditFrameError(
            f"section {section} payload is not a JSON object"
        )
    files = parsed.get("files")
    if not isinstance(files, dict):
        logger.debug(
            "process audit %s snapshot had no 'files' mapping -- mutations will not be audited",
            section,
        )
        return {}
    return {
        str(path): dict(item)
        for path, item in files.items()
        if isinstance(item, dict)
    }


def _parse_process_audit_combined_output(
    raw_stdout: str,
    *,
    run_id: str,
) -> _CombinedExecOutput:
    """Extract the 4 framed sections; raise ``_ProcessAuditFrameError`` on any defect."""
    before_b64 = _extract_section(raw_stdout, run_id, "BEFORE")
    exec_b64 = _extract_section(raw_stdout, run_id, "EXEC")
    exit_plain = _extract_section(raw_stdout, run_id, "EXIT")
    after_b64 = _extract_section(raw_stdout, run_id, "AFTER")

    before = _decode_snapshot(_decode_b64_payload(before_b64, "BEFORE"), "BEFORE")
    after = _decode_snapshot(_decode_b64_payload(after_b64, "AFTER"), "AFTER")
    exec_bytes = _decode_b64_payload(exec_b64, "EXEC")
    exec_stdout = exec_bytes.decode("utf-8", errors="replace")

    exit_stripped = exit_plain.strip()
    try:
        exit_code = int(exit_stripped)
    except ValueError as exc:
        raise _ProcessAuditFrameError(
            f"section EXIT is not an integer: {exit_stripped!r}"
        ) from exc

    return _CombinedExecOutput(
        before=before,
        after=after,
        exec_stdout=exec_stdout,
        exec_exit_code=exit_code,
    )


class ProcessAuditor:
    """Audit workspace mutations around one sandbox process operation."""

    def __init__(
        self,
        *,
        workspace_root: str,
        exec_process: Callable[..., Awaitable[Any]],
        arbiter: Any,
        content: Any,
        symbol_index: Any,
        lsp_client: Any,
    ) -> None:
        self._workspace_root = workspace_root
        self._exec_process = exec_process
        self._arbiter = arbiter
        self._content = content
        self._symbol_index = symbol_index
        self._lsp_client = lsp_client

    async def execute(
        self,
        sandbox: Any,
        command: str,
        *,
        timeout: int | None = None,
        description: str = "",
        agent_id: str = "",
        team_run_id: str = "",
        agent_run_id: str = "",
        task_id: str = "",
        attribute_changes: bool = True,
    ) -> Any:
        """Run one audited sandbox process operation in a single remote exec.

        The before-snapshot, user command, and after-snapshot are all emitted
        by a single bash wrapper script that frames each section with unique
        sentinels. This collapses the old 3-exec sequence into one.

        Contract change vs. the previous try/finally implementation: if the
        outer remote exec raises (container loss, remote timeout) **no audit
        entry is recorded** and the exception propagates. Likewise, if the
        framed output cannot be parsed, ``_ProcessAuditFrameError`` propagates
        and no audit is recorded.
        """
        run_id = uuid.uuid4().hex
        combined = _build_process_audit_combined_bash(
            command,
            workspace_root=self._workspace_root,
            run_id=run_id,
        )
        effective_timeout = timeout + 60 if timeout is not None else None
        combined_response = await self._exec_process(
            sandbox,
            combined,
            timeout=effective_timeout,
        )
        raw = str(getattr(combined_response, "result", "") or "")
        parsed = _parse_process_audit_combined_output(raw, run_id=run_id)
        ambient_changed_paths: list[str] = []
        if attribute_changes:
            changed_paths = self._record_audit(
                before=parsed.before,
                after=parsed.after,
                description=description,
                agent_id=agent_id,
                team_run_id=team_run_id,
                agent_run_id=agent_run_id,
                task_id=task_id,
            )
        else:
            changed_paths = []
            ambient_changed_paths = self._changed_paths_from_snapshots(
                before=parsed.before,
                after=parsed.after,
            )
        return SimpleNamespace(
            result=parsed.exec_stdout,
            exit_code=parsed.exec_exit_code,
            changed_paths=changed_paths,
            ambient_changed_paths=ambient_changed_paths,
            files_written=len(changed_paths),
        )

    def _changed_paths_from_snapshots(
        self,
        *,
        before: dict[str, dict[str, Any]],
        after: dict[str, dict[str, Any]],
    ) -> list[str]:
        changed_paths = sorted(set(before) | set(after))
        recorded_paths: list[str] = []
        for file_path in changed_paths:
            old = before.get(file_path, {})
            new = after.get(file_path, {})
            old_exists = bool(old.get("exists", False))
            new_exists = bool(new.get("exists", False))
            old_hash = str(old.get("hash") or old.get("head_hash") or new.get("head_hash") or "")
            new_hash = str(new.get("hash") or "")
            if not new and old.get("head_hash"):
                new_exists = True
                new_hash = str(old.get("head_hash") or "")
            if old_exists == new_exists and old_hash == new_hash:
                continue
            recorded_paths.append(file_path)
        return recorded_paths

    def _record_audit(
        self,
        *,
        before: dict[str, dict[str, Any]],
        after: dict[str, dict[str, Any]],
        description: str,
        agent_id: str,
        team_run_id: str,
        agent_run_id: str,
        task_id: str,
    ) -> list[str]:
        recorded_paths = self._changed_paths_from_snapshots(before=before, after=after)
        for file_path in recorded_paths:
            old = before.get(file_path, {})
            new = after.get(file_path, {})
            old_exists = bool(old.get("exists", False))
            new_exists = bool(new.get("exists", False))
            old_hash = str(old.get("hash") or old.get("head_hash") or new.get("head_hash") or "")
            new_hash = str(new.get("hash") or "")
            if not new and old.get("head_hash"):
                new_exists = True
                new_hash = str(old.get("head_hash") or "")
            self._arbiter.record_edit(
                file_path=file_path,
                actor_label=agent_id or agent_run_id,
                team_run_id=team_run_id,
                agent_run_id=agent_run_id,
                task_id=task_id,
                old_hash=old_hash if old_exists or old_hash else "",
                new_hash=new_hash if new_exists else "",
                description=description,
            )
            if new_exists:
                try:
                    content, existed = self._content.read(file_path, allow_missing=True)
                    if existed:
                        self._symbol_index.refresh(file_path, content)
                except Exception:
                    logger.debug("symbol refresh failed after process op for %s", file_path, exc_info=True)
            try:
                self._lsp_client.invalidate(file_path)
            except Exception:
                logger.debug("lsp invalidate failed after process op for %s", file_path, exc_info=True)
        return recorded_paths


__all__ = ["ProcessAuditor"]
