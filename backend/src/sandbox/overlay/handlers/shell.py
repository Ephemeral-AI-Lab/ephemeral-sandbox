"""Runtime handler for shell requests after legacy overlay capture removal."""

from __future__ import annotations

from typing import Any

from sandbox.runtime.types import ConflictInfo, ShellResult
from sandbox.runtime.wire import shell_result_to_dict


async def handle(args: dict[str, Any]) -> dict[str, Any]:
    del args
    return shell_result_to_dict(
        ShellResult(
            result="",
            exit_code=1,
            conflict=ConflictInfo(
                reason="overlay_snapshot_required",
                message=(
                    "legacy live-root shell runtime was removed; "
                    "shell mutation requests must use the layer-stack snapshot path"
                ),
            ),
            warnings=(
                "legacy live-root shell runtime was removed",
            ),
        )
    )


__all__ = ["handle"]
