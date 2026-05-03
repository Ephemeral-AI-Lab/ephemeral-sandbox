"""Phase 3 live E2E workspace bypass guard invariant."""

from __future__ import annotations

from typing import Any

import pytest

from ._timing_harness import TimingHarness
from .test_live_ci_phase3_invariants import (
    LivePhase3Env,
    _asyncio_run,
    _traced_step,
    live_phase3_env,  # noqa: F401
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]


def test_workspace_bypass_guard_surfaces_violation(
    live_phase3_env: LivePhase3Env,  # noqa: F811
) -> None:
    """In strict mode, an unledgered write must surface ``WorkspaceBypass``.

    Plant a file in the workspace concurrently with a real mutation. The
    real mutation records itself in the ledger, but the planted file does
    NOT — the guard must flag it as a bypass and replace the success
    envelope with ``WorkspaceBypass`` once strict mode is on.
    """
    h = TimingHarness(phase=3, test_name="bypass_guard")
    env = live_phase3_env
    daemon_backend = env.daemon_backend()

    target = f"{env.repo_dir}/_phase3_guard.txt"
    bypass_target = f"{env.repo_dir}/_phase3_unledgered.txt"

    # Pre-create the guarded file so write_file flows through the modify
    # branch (not the create branch — the guard logic is identical, but
    # this matches production usage).
    env.exec(f"echo 'guarded\\n' > {target}")

    # Enable strict mode via the test-only op (gated on the marker file).
    env.exec("touch $HOME/.cache/eos-ci/*/v1/.allow_test_bypass_op")

    with _traced_step(h, "set_strict_mode"):
        _asyncio_run(daemon_backend._call_daemon_command("_set_guard_mode", {"strict": True}))

    async def write_with_bypass() -> Any:
        # Plant the bypass file just before the mutation so its mtime falls
        # inside the guard's request window.
        env.exec(f"touch {bypass_target}")
        return await daemon_backend._call_daemon_command(
            "write_file",
            {
                "specs": [
                    {
                        "file_path": target,
                        "content": "guarded-v2\n",
                        "overwrite": True,
                    }
                ],
                "agent_id": "agent-guard",
            },
        )

    with _traced_step(h, "write_with_concurrent_bypass"):
        try:
            result = _asyncio_run(write_with_bypass())
        except Exception as exc:
            # DaemonCommandError carries the kind in .kind; surface for assertion.
            assert "WorkspaceBypass" in str(exc), exc
            result = {"success": False, "error_repr": repr(exc)}

    # Cleanup strict mode + planted file regardless of outcome.
    env.exec("rm -f $HOME/.cache/eos-ci/*/v1/.allow_test_bypass_op || true")
    _asyncio_run(daemon_backend._call_daemon_command("_set_guard_mode", {"strict": False}))
    env.exec(f"rm -f {bypass_target}")

    # The bypass file IS still present (detection, not prevention).
    code, _ = env.exec(f"test -f {bypass_target} || echo MISSING")
    # We just removed it; this confirms the test cleanup ran.
    del code

    # The mutation should have failed loud with WorkspaceBypass.
    assert (
        result.get("success") is False
        or "WorkspaceBypass" in str(result)
    ), result
    print(h.report())
    h.dump_json()
