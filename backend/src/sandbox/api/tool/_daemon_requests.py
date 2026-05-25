"""Build daemon request payload fields shared by public sandbox operations."""

from __future__ import annotations

from sandbox._shared.models import SandboxRequestBase


def daemon_identity_payload(request: SandboxRequestBase) -> dict[str, object]:
    payload: dict[str, object] = {
        "agent_id": request.caller.agent_id,
        "caller": request.caller.audit_fields(),
    }
    if request.invocation_id:
        payload["invocation_id"] = request.invocation_id
    return payload
