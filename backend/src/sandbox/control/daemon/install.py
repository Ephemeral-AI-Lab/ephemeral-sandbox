"""Host-side setup orchestration for bundled in-sandbox peers."""

from __future__ import annotations

import shlex
from dataclasses import dataclass
from pathlib import PurePosixPath
from typing import Protocol

from sandbox.api import RawExecResult
from sandbox.control.daemon.bundle import BUNDLE_REMOTE_DIR as _BUNDLE_REMOTE_DIR


class _RawExecCallable(Protocol):
    async def __call__(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult: ...


class _EnsureUploadedCallable(Protocol):
    async def __call__(self, sandbox_id: str) -> str: ...


@dataclass(frozen=True)
class SetupScript:
    """Peer-owned bundled setup script.

    ``relative_path`` is the path inside the extracted runtime bundle, for
    example ``sandbox/runtime/peer/setup.sh``.
    """

    name: str
    package: str
    relative_path: str

    def __post_init__(self) -> None:
        if not self.name:
            raise ValueError("setup script name must be non-empty")
        if not self.package:
            raise ValueError("setup script package must be non-empty")
        path = PurePosixPath(self.relative_path)
        if path.is_absolute() or ".." in path.parts:
            raise ValueError("setup script path must stay inside the bundle")
        if path.name != "setup.sh":
            raise ValueError("setup script path must point to setup.sh")


class SetupRegistry:
    """Ordered registry of peer setup scripts."""

    def __init__(self) -> None:
        self._scripts: list[SetupScript] = []

    def register(self, setup_script: SetupScript) -> None:
        for script in self._scripts:
            if script.name != setup_script.name:
                continue
            if script == setup_script:
                return
            raise ValueError(f"setup script already registered: {setup_script.name}")
        self._scripts.append(setup_script)

    async def run_all(
        self,
        sandbox_id: str,
        *,
        exec_fn: _RawExecCallable | None = None,
        ensure_uploaded: _EnsureUploadedCallable | None = None,
    ) -> list[RawExecResult]:
        """Upload the runtime bundle and run registered setup scripts in order."""
        if not self._scripts:
            return []

        exec_fn = exec_fn or _raw_exec
        ensure_uploaded = ensure_uploaded or _ensure_runtime_uploaded
        await ensure_uploaded(sandbox_id)
        results: list[RawExecResult] = []
        for script in self._scripts:
            command = f"bash {shlex.quote(script.relative_path)}"
            result = await exec_fn(
                sandbox_id,
                command,
                cwd=_BUNDLE_REMOTE_DIR,
                timeout=300,
            )
            if result.exit_code != 0:
                raise RuntimeError(
                    f"setup script {script.name!r} failed in sandbox "
                    f"{sandbox_id!r}: {result.stderr or result.stdout}"
                )
            results.append(result)
        return results

    @property
    def scripts(self) -> tuple[SetupScript, ...]:
        return tuple(self._scripts)


_REGISTRY = SetupRegistry()


async def _raw_exec(
    sandbox_id: str,
    command: str,
    *,
    cwd: str | None = None,
    timeout: int | None = None,
) -> RawExecResult:
    from sandbox.api.raw_exec import raw_exec

    return await raw_exec(sandbox_id, command, cwd=cwd, timeout=timeout)


async def _ensure_runtime_uploaded(sandbox_id: str) -> str:
    from sandbox.control.daemon.bundle import ensure_runtime_uploaded

    return await ensure_runtime_uploaded(sandbox_id)


def register(setup_script: SetupScript) -> None:
    _REGISTRY.register(setup_script)


async def run_all(sandbox_id: str) -> list[RawExecResult]:
    return await _REGISTRY.run_all(sandbox_id)


__all__ = ["SetupRegistry", "SetupScript", "register", "run_all"]
