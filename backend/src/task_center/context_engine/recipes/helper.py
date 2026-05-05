"""Helper recipes — ``advisor_v1`` and ``resolver_v1``.

Helper agents (advisor, resolver) are spawned by parent agents via tools
(``ask_advisor`` / ``run_subagent``). They inherit the parent's full
:class:`ContextPacket` so they reason inside the parent's frame, not in
isolation. See plan §3.3.8.

Inheritance policy: every parent block is copied with priority demoted by
exactly one level (``required → high → medium → low → low``). Inherited blocks
carry ``metadata['inherited_from_parent'] = 'true'`` so the renderer can group
them under a ``# Parent context`` heading. The concrete helper request is
appended by the helper tool after composition.
"""

from __future__ import annotations

from task_center.context_engine.engine import ContextEngineDeps
from task_center.context_engine.errors import ContextEngineError
from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center.context_engine.recipes_registry import ContextRecipe
from task_center.context_engine.scope import ContextScope

ADVISOR_V1 = "advisor_v1"
RESOLVER_V1 = "resolver_v1"

_HELPER_REQUIRED_FIELDS = frozenset(
    {"request_id", "task_id", "parent_packet_id"}
)

_DEMOTION = {
    ContextPriority.REQUIRED: ContextPriority.HIGH,
    ContextPriority.HIGH: ContextPriority.MEDIUM,
    ContextPriority.MEDIUM: ContextPriority.LOW,
    ContextPriority.LOW: ContextPriority.LOW,
}


def demote_priority(priority: ContextPriority) -> ContextPriority:
    return _DEMOTION[priority]


def _build_helper_packet(
    *,
    target_role: str,
    scope: ContextScope,
    deps: ContextEngineDeps,
) -> ContextPacket:
    if deps.context_packet_store is None:
        raise ContextEngineError(
            "Helper recipes require ContextEngineDeps.context_packet_store; "
            "wire ContextPacketStore through app startup."
        )
    parent_packet = deps.context_packet_store.get(scope.parent_packet_id)
    if parent_packet is None:
        raise ContextEngineError(
            f"Parent packet {scope.parent_packet_id!r} not found"
        )
    blocks: list[ContextBlock] = []
    for parent_block in parent_packet.blocks:
        demoted = demote_priority(parent_block.priority)
        inherited_meta = {
            **parent_block.metadata,
            "inherited_from_parent": "true",
        }
        blocks.append(
            ContextBlock(
                kind=parent_block.kind,
                priority=demoted,
                text=parent_block.text,
                source_id=parent_block.source_id,
                source_kind=parent_block.source_kind,
                metadata=inherited_meta,
            )
        )
    return ContextPacket(
        target_role=target_role,
        target_id=scope.task_id,
        canonical_refs=ContextRefs(
            request_id=scope.request_id,
            task_id=scope.task_id,
        ),
        blocks=blocks,
        metadata={"inherits_from": parent_packet.id},
        source_ids=[b.source_id for b in blocks if b.source_id],
    )


def _advisor_v1_build(
    scope: ContextScope, deps: ContextEngineDeps
) -> ContextPacket:
    return _build_helper_packet(
        target_role="advisor", scope=scope, deps=deps
    )


def _resolver_v1_build(
    scope: ContextScope, deps: ContextEngineDeps
) -> ContextPacket:
    return _build_helper_packet(
        target_role="resolver", scope=scope, deps=deps
    )


ADVISOR_V1_RECIPE = ContextRecipe(
    id=ADVISOR_V1,
    required_scope_fields=_HELPER_REQUIRED_FIELDS,
    build=_advisor_v1_build,
)

RESOLVER_V1_RECIPE = ContextRecipe(
    id=RESOLVER_V1,
    required_scope_fields=_HELPER_REQUIRED_FIELDS,
    build=_resolver_v1_build,
)


__all__ = [
    "ADVISOR_V1",
    "ADVISOR_V1_RECIPE",
    "RESOLVER_V1",
    "RESOLVER_V1_RECIPE",
    "demote_priority",
]
