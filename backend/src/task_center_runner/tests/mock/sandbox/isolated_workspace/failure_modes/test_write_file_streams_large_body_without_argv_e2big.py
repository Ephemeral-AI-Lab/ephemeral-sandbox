"""5 MB write_file body must NOT trip argv E2BIG.

Cross-ref to project memory ``'checked batch apply failed' = argv
E2BIG``. The iws ``api.v1.write_file`` path sends the tool payload through
the existing namespace runner's stdin protocol; argv stays bounded by the
small Python entrypoint wrapper. A 5 MB body therefore must succeed
end-to-end and the readback content must match.

We pass a TEXT body (5 MB of repeated ASCII) rather than random bytes —
the property under test is argv size, not binary integrity. The current
``_iws_rpc.write_file`` client double-encodes binary input via base64,
which would mangle the readback for an unrelated protocol reason (filed
as a separate follow-up in NEXT-AGENT-GUIDE deferred items).
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(240)
async def test_write_file_streams_large_body_without_argv_e2big(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id,
        "agent-A",
        layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        # 5 MB ASCII body. The argv limit on Linux is ~128 KB by default;
        # this is ~40× over so the stdin-streaming path is the only way
        # the namespace entrypoint can receive the payload.
        body_chunk = "x" * 1024
        body = body_chunk * (5 * 1024)
        assert len(body) == 5 * 1024 * 1024

        path = "/testbed/big-body.txt"
        write = await _iws_rpc.write_file(sandbox_id, "agent-A", path, body)
        assert write.get("success") is True, write

        # Read back size + a sentinel byte.
        size = await _iws_rpc.shell(
            sandbox_id,
            "agent-A",
            f"wc -c < {path}",
        )
        assert size.get("success") is True, size
        assert "5242880" in (size.get("stdout", "") or ""), size

        head = await _iws_rpc.shell(
            sandbox_id,
            "agent-A",
            f"head -c 16 {path}",
        )
        assert "x" * 16 in (head.get("stdout", "") or ""), head
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
