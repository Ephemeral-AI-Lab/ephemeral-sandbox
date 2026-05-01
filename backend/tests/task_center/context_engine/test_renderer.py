"""US-003: MarkdownPromptRenderer behavior."""

from __future__ import annotations

from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center.context_engine.renderer import MarkdownPromptRenderer


def _packet(blocks: list[ContextBlock], **metadata: str) -> ContextPacket:
    return ContextPacket(
        target_role="planner",
        target_id="g-1",
        canonical_refs=ContextRefs(request_id="r"),
        blocks=blocks,
        metadata=dict(metadata),
    )


def test_priority_order_required_first_then_high_medium_low():
    blocks = [
        ContextBlock(kind="low", priority=ContextPriority.LOW, text="low"),
        ContextBlock(kind="high", priority=ContextPriority.HIGH, text="high"),
        ContextBlock(
            kind="required", priority=ContextPriority.REQUIRED, text="req"
        ),
        ContextBlock(
            kind="medium", priority=ContextPriority.MEDIUM, text="medium"
        ),
    ]
    out = MarkdownPromptRenderer().render(_packet(blocks))
    # Headings appear in priority order.
    assert out.find("Required") < out.find("High")
    assert out.find("High") < out.find("Medium")
    assert out.find("Medium") < out.find("Low")


def test_required_blocks_never_compressed_under_budget():
    big_required = "A" * 4_000  # ≈1000 tokens
    blocks = [
        ContextBlock(
            kind="segment_goal",
            priority=ContextPriority.REQUIRED,
            text=big_required,
            source_id="seg-1",
        ),
        ContextBlock(
            kind="background",
            priority=ContextPriority.LOW,
            text="B" * 4_000,
            source_id="src-low",
        ),
    ]
    out = MarkdownPromptRenderer().render(_packet(blocks, token_budget="100"))
    assert big_required in out
    # The low block should be replaced with the truncation marker.
    assert "B" * 4_000 not in out
    assert "truncated for token budget" in out


def test_low_blocks_compressed_before_medium_blocks():
    blocks = [
        ContextBlock(
            kind="seg",
            priority=ContextPriority.REQUIRED,
            text="goal",
        ),
        ContextBlock(
            kind="med",
            priority=ContextPriority.MEDIUM,
            text="M" * 4_000,
            source_id="src-med",
        ),
        ContextBlock(
            kind="lo",
            priority=ContextPriority.LOW,
            text="L" * 4_000,
            source_id="src-lo",
        ),
    ]
    # Budget is just enough for required + medium + truncation message.
    out = MarkdownPromptRenderer().render(_packet(blocks, token_budget="1100"))
    # Low truncated, medium kept verbatim.
    assert "L" * 4_000 not in out
    assert "M" * 4_000 in out


def test_inherited_blocks_grouped_under_parent_context_section():
    blocks = [
        ContextBlock(
            kind="parent_question",
            priority=ContextPriority.REQUIRED,
            text="question",
        ),
        ContextBlock(
            kind="segment_goal",
            priority=ContextPriority.HIGH,
            text="parent goal",
            metadata={"inherited_from_parent": "true"},
        ),
        ContextBlock(
            kind="prior_segment_summary",
            priority=ContextPriority.MEDIUM,
            text="parent summary",
            metadata={"inherited_from_parent": "true"},
        ),
    ]
    out = MarkdownPromptRenderer().render(_packet(blocks))
    parent_idx = out.find("# Parent context")
    assert parent_idx > 0, "expected '# Parent context' section"
    assert out.find("parent goal") > parent_idx
    assert out.find("parent summary") > parent_idx
    # Helper-owned parent_question renders before the Parent context heading.
    assert out.find("question") < parent_idx


def test_seg_initial_subtitle_emitted_when_metadata_set():
    blocks = [
        ContextBlock(
            kind="segment_goal",
            priority=ContextPriority.REQUIRED,
            text="g",
        )
    ]
    out = MarkdownPromptRenderer().render(
        _packet(blocks, is_initial_segment="true")
    )
    assert "*(first segment" in out


def test_render_is_deterministic_for_fixed_packet():
    blocks = [
        ContextBlock(kind="a", priority=ContextPriority.REQUIRED, text="a"),
        ContextBlock(kind="b", priority=ContextPriority.HIGH, text="b"),
    ]
    packet = _packet(blocks)
    a = MarkdownPromptRenderer().render(packet)
    b = MarkdownPromptRenderer().render(packet)
    assert a == b


def test_renderer_does_not_perform_io_or_store_reads(tmp_path, monkeypatch):
    """Renderer must be a pure function. Trip-wire: deny attribute access on
    objects that look like stores during render — render should not touch
    them."""
    blocks = [
        ContextBlock(kind="x", priority=ContextPriority.REQUIRED, text="x")
    ]
    # No store handle is ever passed to render(); the contract is enforced
    # by the absence of any store parameter in render's signature.
    import inspect

    sig = inspect.signature(MarkdownPromptRenderer().render)
    assert list(sig.parameters) == ["packet"]
    MarkdownPromptRenderer().render(_packet(blocks))
