"""US-019: token-budget compression contract.

Required blocks must never be compressed. Low blocks compress before medium
blocks. Output is deterministic for a fixed packet.
"""

from __future__ import annotations

from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center.context_engine.renderer import MarkdownPromptRenderer


def _packet_with_budget(blocks: list[ContextBlock], budget: int) -> ContextPacket:
    return ContextPacket(
        target_role="planner",
        target_id="g",
        canonical_refs=ContextRefs(request_id="r"),
        blocks=blocks,
        metadata={"token_budget": str(budget)},
    )


def test_required_blocks_kept_byte_for_byte_under_pressure():
    big_required_a = ("REQ-A_" * 1_000)
    big_required_b = ("REQ-B_" * 1_000)
    packet = _packet_with_budget(
        [
            ContextBlock(
                kind="segment_goal",
                priority=ContextPriority.REQUIRED,
                text=big_required_a,
            ),
            ContextBlock(
                kind="complex_task_goal",
                priority=ContextPriority.REQUIRED,
                text=big_required_b,
            ),
            ContextBlock(
                kind="lo",
                priority=ContextPriority.LOW,
                text="L" * 8_000,
                source_id="src-lo",
            ),
            ContextBlock(
                kind="med",
                priority=ContextPriority.MEDIUM,
                text="M" * 8_000,
                source_id="src-med",
            ),
        ],
        budget=50,  # very tight budget
    )
    out = MarkdownPromptRenderer().render(packet)
    assert big_required_a in out, "required block A must survive verbatim"
    assert big_required_b in out, "required block B must survive verbatim"


def test_low_blocks_truncate_before_medium_when_budget_allows_medium():
    """Compression order: low first, then medium. Required + high never."""
    packet = _packet_with_budget(
        [
            ContextBlock(
                kind="seg",
                priority=ContextPriority.REQUIRED,
                text="goal text",
            ),
            ContextBlock(
                kind="med",
                priority=ContextPriority.MEDIUM,
                text=("MED-keep_" * 200),
                source_id="src-med",
            ),
            ContextBlock(
                kind="lo",
                priority=ContextPriority.LOW,
                text=("LOW-drop_" * 1_000),
                source_id="src-lo",
            ),
        ],
        budget=600,
    )
    out = MarkdownPromptRenderer().render(packet)
    assert ("LOW-drop_" * 1_000) not in out, "low block should be truncated"
    assert ("MED-keep_" * 200) in out, "medium block should survive"


def test_render_output_is_deterministic_for_fixed_packet():
    blocks = [
        ContextBlock(
            kind="seg",
            priority=ContextPriority.REQUIRED,
            text="A",
        ),
        ContextBlock(
            kind="lo",
            priority=ContextPriority.LOW,
            text="B" * 1000,
            source_id="src",
        ),
    ]
    packet = _packet_with_budget(blocks, budget=100)
    a = MarkdownPromptRenderer().render(packet)
    b = MarkdownPromptRenderer().render(packet)
    assert a == b


def test_high_priority_blocks_kept_when_only_low_medium_present_to_truncate():
    """Per plan §3.4: never compress required or high. Only low + medium drop."""
    packet = _packet_with_budget(
        [
            ContextBlock(
                kind="seg",
                priority=ContextPriority.REQUIRED,
                text="REQ",
            ),
            ContextBlock(
                kind="hi",
                priority=ContextPriority.HIGH,
                text=("HIGH-keep_" * 500),
            ),
            ContextBlock(
                kind="lo",
                priority=ContextPriority.LOW,
                text="L" * 4_000,
                source_id="src",
            ),
        ],
        budget=300,
    )
    out = MarkdownPromptRenderer().render(packet)
    assert ("HIGH-keep_" * 500) in out, "high block must not be truncated"
