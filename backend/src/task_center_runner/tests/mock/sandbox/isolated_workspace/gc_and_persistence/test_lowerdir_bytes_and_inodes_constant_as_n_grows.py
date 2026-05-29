"""Lowerdir bytes AND inodes stay constant as N concurrent workspaces grow.

Sharpens the existing disk-O(1) coverage (which checks a single N=5 byte
ceiling over ``_REPO_DIR``) into the explicit invariant the scenario plan
asks for:

  * The probe is **lowerdir-subtree-scoped** to ``<layer_stack_root>/layers``
    (the shared layer content overlayfs uses as its lowerdir), not the whole
    workspace root, so a per-handle tree-copy regression shows up as a hard
    delta rather than hiding under a 10 MB/handle slack budget.
  * Both **bytes** (``du -sb``) and **inode count** (``du --inodes -s``) are
    asserted constant across ``N ∈ {1, 3, 5}`` — a tree-copy that shared
    bytes via reflink but duplicated inodes would still be caught.
  * The per-handle uppers (under the scratch root) are asserted to grow
    ``≈ N × M`` over the same window, proving the cost moved to the upper and
    only the upper.

Companion structural backstops:
``test_lowerdir_layer_paths_shared_across_concurrent_handles`` (manifest +
``tree-copy: False`` audit) and ``test_lowerdir_disk_usage_is_o1`` (N=5 byte
ceiling).
"""

from __future__ import annotations

import asyncio

import pytest

from sandbox.api import raw_exec
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    iws_scratch_root,
)


pytestmark = pytest.mark.asyncio

_AGENTS = ("agent-A", "agent-B", "agent-C", "agent-D", "agent-E")
_N_SERIES = (1, 3, 5)
_FILL_MIB = 8
_LOWERDIR = f"{_iws_rpc.IWS_LAYER_STACK_ROOT}/layers"


async def _du_first_col(sandbox_id: str, command: str) -> int:
    result = await raw_exec(sandbox_id, command, cwd="/", timeout=60)
    text = (getattr(result, "stdout", "") or "").strip()
    return int(text) if text.isdigit() else -1


async def _lower_bytes(sandbox_id: str) -> int:
    return await _du_first_col(
        sandbox_id, f"du -sb {_LOWERDIR} 2>/dev/null | awk '{{print $1}}'"
    )


async def _lower_inodes(sandbox_id: str) -> int:
    return await _du_first_col(
        sandbox_id, f"du --inodes -s {_LOWERDIR} 2>/dev/null | awk '{{print $1}}'"
    )


async def _upper_bytes(sandbox_id: str, scratch_root: str) -> int:
    # Sum every handle's upper/ subtree; the manager.json sibling is bytes-noise.
    return await _du_first_col(
        sandbox_id,
        f"du -sb {scratch_root}/*/upper 2>/dev/null | awk '{{s+=$1}} END {{print s+0}}'",
    )


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(600)
async def test_lowerdir_bytes_and_inodes_constant_as_n_grows(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    scratch = await iws_scratch_root(sandbox_id)
    assert scratch, "iws scratch_root not discovered"

    baseline_bytes = await _lower_bytes(sandbox_id)
    baseline_inodes = await _lower_inodes(sandbox_id)
    assert baseline_bytes > 0, f"lowerdir du returned {baseline_bytes} — path mismatch?"
    assert baseline_inodes > 0, f"lowerdir inode du returned {baseline_inodes}"

    fill_floor = int(_FILL_MIB * 1024 * 1024 * 0.8)

    for n in _N_SERIES:
        agents = _AGENTS[:n]
        enters = await asyncio.gather(
            *(
                _iws_rpc.enter(
                    sandbox_id, agent, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT
                )
                for agent in agents
            )
        )
        try:
            assert all(r.get("success") for r in enters), (n, enters)
            # Each workspace writes M MiB into its own upper.
            fills = await asyncio.gather(
                *(
                    _iws_rpc.shell(
                        sandbox_id,
                        agent,
                        f"dd if=/dev/zero of=/testbed/iws-fill.bin "
                        f"bs=1M count={_FILL_MIB} status=none",
                    )
                    for agent in agents
                )
            )
            assert all(r.get("success") for r in fills), (n, fills)

            lower_bytes = await _lower_bytes(sandbox_id)
            lower_inodes = await _lower_inodes(sandbox_id)
            upper_bytes = await _upper_bytes(sandbox_id, scratch)

            assert lower_bytes == baseline_bytes, (
                f"lowerdir bytes grew with N={n}: baseline={baseline_bytes} "
                f"now={lower_bytes} (per-handle tree-copy regression?)"
            )
            assert lower_inodes == baseline_inodes, (
                f"lowerdir inodes grew with N={n}: baseline={baseline_inodes} "
                f"now={lower_inodes} (per-handle tree-copy regression?)"
            )
            # The cost moved to the upper: ~N×M landed across the open uppers.
            assert upper_bytes >= n * fill_floor, (
                f"upper bytes did not scale with N={n}: upper={upper_bytes} "
                f"expected >= {n * fill_floor} (≈{n}×{_FILL_MIB}MiB)"
            )
        finally:
            for agent in agents:
                await _iws_rpc.exit_(sandbox_id, agent)

    # The lowerdir is unchanged after every cohort exits.
    assert await _lower_bytes(sandbox_id) == baseline_bytes
    assert await _lower_inodes(sandbox_id) == baseline_inodes
