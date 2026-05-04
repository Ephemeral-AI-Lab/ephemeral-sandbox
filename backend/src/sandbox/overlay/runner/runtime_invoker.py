"""Runtime invocation for one leased snapshot overlay shell call."""

from __future__ import annotations

import asyncio
from pathlib import Path
from uuid import uuid4

from sandbox.layer_stack.manifest import Manifest
from sandbox.overlay.types import (
    OverlayShellRequest,
    overlay_shell_request_to_dict,
)
from sandbox.runtime.overlay_shell.cli import execute_request
from sandbox.runtime.overlay_shell.result_envelope import RuntimeResultEnvelope


class RuntimeInvoker:
    """Invoke the runtime-local overlay shell command and return its envelope."""

    def __init__(
        self,
        *,
        storage_root: str | Path,
        runtime_root: str | Path | None = None,
    ) -> None:
        self.storage_root = Path(storage_root)
        self.runtime_root = Path(runtime_root) if runtime_root is not None else (
            self.storage_root / "runtime" / "overlay_shell"
        )

    async def invoke(
        self,
        *,
        request: OverlayShellRequest,
        manifest: Manifest,
    ) -> RuntimeResultEnvelope:
        run_dir = self._run_dir(request)
        return await asyncio.to_thread(
            execute_request,
            request_payload=overlay_shell_request_to_dict(request),
            manifest_payload=manifest.to_dict(),
            storage_root=self.storage_root,
            run_dir=run_dir,
        )

    def _run_dir(self, request: OverlayShellRequest) -> Path:
        safe_id = "".join(
            char if char.isalnum() or char in ("-", "_") else "-"
            for char in request.request_id
        ).strip("-")
        suffix = uuid4().hex[:8]
        return self.runtime_root / f"{safe_id or 'request'}-{suffix}"


__all__ = ["RuntimeInvoker"]
