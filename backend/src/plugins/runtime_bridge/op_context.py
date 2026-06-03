"""Context objects passed to in-sandbox plugin op handlers."""

from __future__ import annotations

from collections.abc import Callable, Mapping
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Protocol

from sandbox._shared.models import Intent, SandboxCaller

if TYPE_CHECKING:
    from sandbox.ephemeral_workspace.events import WorkspaceChangeEvent
    from sandbox.overlay.handle import OverlayHandle
else:
    WorkspaceChangeEvent = Any
    OverlayHandle = Any

__all__ = [
    "PluginOpContext",
    "WorkspaceChangeEvent",
    "WorkspaceProjectionLike",
    "plugin_intent_from_envelope",
    "sandbox_caller_from_plugin_envelope",
]

AuditFieldReader = Callable[[Mapping[str, Any], str], str]
_CALLER_AUDIT_FIELDS = (
    "agent_id",
    "run_id",
    "agent_run_id",
    "task_id",
    "request_id",
    "attempt_id",
    "workflow_id",
    "tool_name",
    "tool_id",
)


def sandbox_caller_from_plugin_envelope(
    payload: object,
    *,
    field_reader: AuditFieldReader | None = None,
) -> SandboxCaller:
    if not isinstance(payload, Mapping):
        return SandboxCaller(agent_id="")
    read_field = field_reader or _audit_payload_field
    return SandboxCaller(
        **{field: read_field(payload, field) for field in _CALLER_AUDIT_FIELDS}
    )


def plugin_intent_from_envelope(value: object) -> Intent:
    raw = str(value or "")
    if not raw:
        return Intent.READ_ONLY
    try:
        return Intent(raw)
    except ValueError:
        return Intent.READ_ONLY


def _audit_payload_field(payload: Mapping[str, Any], key: str) -> str:
    return str(payload.get(key) or "")


class WorkspaceProjectionLike(Protocol):
    @property
    def layer_stack_root(self) -> Any: ...

    def acquire(self, invocation_id: str) -> Any: ...

    def acquire_overlay(
        self,
        invocation_id: str,
        *,
        workspace_root: str,
    ) -> OverlayHandle: ...

    def active_manifest_key(self) -> str: ...


@dataclass(frozen=True)
class PluginOpContext:
    """Plugin handler context.

    READ_ONLY handlers must query a PluginService instead of doing direct
    filesystem I/O.

    ``overlay`` is the daemon-side ``EphemeralPipeline`` (or a test double).
    Typed as ``Any`` because handlers reach for attributes via ``getattr`` —
    a static Protocol added no enforcement and drifted as the pipeline grew.
    """

    layer_stack_root: str
    caller: SandboxCaller
    projection: WorkspaceProjectionLike
    overlay: Any
    intent: Intent = Intent.READ_ONLY
    metadata: dict[str, Any] = field(default_factory=dict)
