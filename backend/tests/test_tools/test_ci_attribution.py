"""Tests for the ``tools.core.ci_attribution`` helpers.

The focus here is ``rebind_ci_service``: it is the load-bearing shim the
typed-API tool call sites rely on to keep a worker-thread OCC call
pointed at the context's current sandbox (after a ``_recover_sandbox``
reattach, for example). Its two silent-no-op branches — no sandbox in
metadata, or ``svc`` without ``rebind_sandbox`` — are part of the
contract; this test locks them in so a future refactor cannot drop
them without being caught.
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock

from tools.core.base import ToolExecutionContextService
from tools.core.ci_attribution import rebind_ci_service


def _context(**metadata) -> ToolExecutionContextService:
    return ToolExecutionContextService(cwd=Path("/tmp/rebind-test"), services=metadata)


def test_rebind_ci_service_forwards_sandbox_to_service() -> None:
    sandbox = SimpleNamespace(id="sandbox-A")
    svc = MagicMock()
    ctx = _context(daytona_sandbox=sandbox)

    rebind_ci_service(ctx, svc)

    svc.rebind_sandbox.assert_called_once_with(sandbox)


def test_rebind_ci_service_noops_when_context_has_no_sandbox() -> None:
    svc = MagicMock()
    ctx = _context()

    rebind_ci_service(ctx, svc)

    svc.rebind_sandbox.assert_not_called()


def test_rebind_ci_service_noops_when_svc_lacks_rebind_sandbox() -> None:
    sandbox = SimpleNamespace(id="sandbox-A")

    class _NoRebindService:
        """Stand-in for a service that never learned to rebind."""

    svc = _NoRebindService()
    ctx = _context(daytona_sandbox=sandbox)

    # Must not raise: the helper silently no-ops when the service does
    # not expose ``rebind_sandbox``.
    rebind_ci_service(ctx, svc)
