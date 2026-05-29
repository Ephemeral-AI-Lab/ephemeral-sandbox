"""XmlPromptRenderer behavior."""

from __future__ import annotations

import pytest

from task_center.context_engine.exceptions import ContextEngineError
from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
    ContextRefs,
)
from task_center.context_engine.renderer import XmlPromptRenderer


def _packet(blocks: list[ContextBlock], **metadata: str) -> ContextPacket:
    return ContextPacket(
        target_role="planner",
        target_id="g-1",
        canonical_refs=ContextRefs(workflow_id="r"),
        blocks=blocks,
        metadata=dict(metadata),
    )


def test_packet_order_is_semantic_order_not_priority_order():
    blocks = [
        ContextBlock(
            kind="low",
            priority=ContextPriority.LOW,
            text="low body",
            metadata={"tag": "low"},
        ),
        ContextBlock(
            kind="high",
            priority=ContextPriority.HIGH,
            text="high body",
            metadata={"tag": "high"},
        ),
        ContextBlock(
            kind="required",
            priority=ContextPriority.REQUIRED,
            text="required body",
            metadata={"tag": "required"},
        ),
        ContextBlock(
            kind="medium",
            priority=ContextPriority.MEDIUM,
            text="medium body",
            metadata={"tag": "medium"},
        ),
    ]
    out = XmlPromptRenderer().render_context(_packet(blocks))
    assert out.find("low body") < out.find("high body")
    assert out.find("high body") < out.find("required body")
    assert out.find("required body") < out.find("medium body")


def test_required_blocks_never_compressed_under_budget():
    big_required = "A" * 4_000  # ≈1000 tokens
    blocks = [
        ContextBlock(
            kind="iteration_statement",
            priority=ContextPriority.REQUIRED,
            text=big_required,
            source_id="seg-1",
        ),
        ContextBlock(
            kind="background",
            priority=ContextPriority.LOW,
            text="B" * 4_000,
            source_id="src-low",
            metadata={"tag": "background"},
        ),
    ]
    out = XmlPromptRenderer().render_context(_packet(blocks, token_budget="100"))
    assert big_required in out
    assert "B" * 4_000 not in out
    assert "truncated for token budget" in out


def test_low_blocks_compressed_before_medium_blocks():
    blocks = [
        ContextBlock(
            kind="iteration_statement",
            priority=ContextPriority.REQUIRED,
            text="goal",
        ),
        ContextBlock(
            kind="medium_kind",
            priority=ContextPriority.MEDIUM,
            text="M" * 4_000,
            source_id="src-med",
            metadata={"tag": "medium_kind"},
        ),
        ContextBlock(
            kind="low_kind",
            priority=ContextPriority.LOW,
            text="L" * 4_000,
            source_id="src-lo",
            metadata={"tag": "low_kind"},
        ),
    ]
    out = XmlPromptRenderer().render_context(_packet(blocks, token_budget="1100"))
    assert "L" * 4_000 not in out
    assert "M" * 4_000 in out


def test_block_tag_metadata_overrides_default_tag():
    blocks = [
        ContextBlock(
            kind="goal_statement",
            priority=ContextPriority.REQUIRED,
            text="body",
            metadata={"tag": "custom_tag"},
        )
    ]
    out = XmlPromptRenderer().render_context(_packet(blocks))
    assert out.startswith("<custom_tag>\nbody\n</custom_tag>")
    assert "<goal>" not in out


def test_block_attrs_metadata_renders_as_xml_attributes():
    blocks = [
        ContextBlock(
            kind="goal_statement",
            priority=ContextPriority.REQUIRED,
            text="body",
            metadata={"tag": "goal", "attrs": 'iteration_no="1" status="current"'},
        )
    ]
    out = XmlPromptRenderer().render_context(_packet(blocks))
    assert '<goal iteration_no="1" status="current">' in out
    assert "</goal>" in out


def test_unknown_kind_without_tag_metadata_raises():
    blocks = [
        ContextBlock(kind="unknown_kind", priority=ContextPriority.REQUIRED, text="x")
    ]
    with pytest.raises(ContextEngineError, match="No tag mapping for kind"):
        XmlPromptRenderer().render_context(_packet(blocks))


def test_grouped_blocks_render_as_one_xml_parent_with_children():
    blocks = [
        ContextBlock(
            kind="dependency_summary",
            priority=ContextPriority.MEDIUM,
            text="dep output",
            metadata={
                "group_id": "deps-1",
                "group_tag": "dependency_results",
                "child_tag": "dependency",
                "attrs": 'id="dep-a"',
            },
        ),
        ContextBlock(
            kind="dependency_summary",
            priority=ContextPriority.MEDIUM,
            text="other dep",
            metadata={
                "group_id": "deps-1",
                "group_tag": "dependency_results",
                "child_tag": "dependency",
                "attrs": 'id="dep-b"',
            },
        ),
    ]
    out = XmlPromptRenderer().render_context(_packet(blocks))
    assert out.count("<dependency_results>") == 1
    assert out.count("</dependency_results>") == 1
    assert '<dependency id="dep-a">' in out
    assert '<dependency id="dep-b">' in out


def test_group_attrs_render_on_parent_tag():
    blocks = [
        ContextBlock(
            kind="prior_iteration_specification",
            priority=ContextPriority.HIGH,
            text="iteration plan",
            metadata={
                "group_id": "iter-1",
                "group_tag": "iteration",
                "group_attrs": 'iteration_no="1" status="prior"',
                "child_tag": "accepted_plan",
            },
        )
    ]
    out = XmlPromptRenderer().render_context(_packet(blocks))
    assert '<iteration iteration_no="1" status="prior">' in out


def test_render_is_deterministic_for_fixed_packet():
    blocks = [
        ContextBlock(
            kind="goal_statement",
            priority=ContextPriority.REQUIRED,
            text="a",
            metadata={"tag": "goal"},
        ),
        ContextBlock(
            kind="evaluation_criteria",
            priority=ContextPriority.HIGH,
            text="b",
            metadata={"tag": "evaluation_criteria"},
        ),
    ]
    packet = _packet(blocks)
    a = XmlPromptRenderer().render_context(packet)
    b = XmlPromptRenderer().render_context(packet)
    assert a == b


def test_renderer_does_not_perform_io_or_store_reads():
    """Renderer must be a pure function — no store handle threaded in."""
    import inspect

    sig = inspect.signature(XmlPromptRenderer().render_context)
    assert list(sig.parameters) == ["packet"]
    blocks = [
        ContextBlock(
            kind="goal_statement",
            priority=ContextPriority.REQUIRED,
            text="x",
        )
    ]
    XmlPromptRenderer().render_context(_packet(blocks))


# ---------------------------------------------------------------------------
# Verbatim contract — no .strip(), whitespace preserved byte-for-byte.
# ---------------------------------------------------------------------------


def test_renderer_preserves_leading_and_trailing_whitespace_verbatim():
    body = "  leading spaces\n\nblank line above\nand trailing newlines\n\n\n"
    blocks = [
        ContextBlock(
            kind="goal_statement",
            priority=ContextPriority.REQUIRED,
            text=body,
            metadata={"tag": "goal"},
        )
    ]
    out = XmlPromptRenderer().render_context(_packet(blocks))
    assert f"<goal>\n{body}\n</goal>" in out


def test_renderer_preserves_fenced_code_block_indentation():
    body = "```\n    indented code line\n    second line\n```"
    blocks = [
        ContextBlock(
            kind="goal_statement",
            priority=ContextPriority.REQUIRED,
            text=body,
            metadata={"tag": "goal"},
        )
    ]
    out = XmlPromptRenderer().render_context(_packet(blocks))
    assert body in out


# ---------------------------------------------------------------------------
# Hostile-body guard: block text containing a structural closer is rejected.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "closer,tag",
    [
        ("</goal>", "goal"),
        ("</attempt_plan>", "attempt_plan"),
        ("</iteration>", "iteration"),
        ("</plan_spec>", "plan_spec"),
        ("</deferred_goal_for_next_iteration>", "deferred_goal_for_next_iteration"),
        ("</evaluation_criteria>", "evaluation_criteria"),
        ("</completed_tasks>", "completed_tasks"),
        ("</dependency_results>", "dependency_results"),
        ("</assigned_task>", "assigned_task"),
    ],
)
def test_hostile_body_with_structural_closer_raises(closer: str, tag: str):
    """Per-closer parametrization mirrors plan §9 unit-test list."""
    hostile = f"normal content {closer} more content"
    blocks = [
        ContextBlock(
            kind="goal_statement",
            priority=ContextPriority.REQUIRED,
            text=hostile,
            source_id="hostile-src",
            metadata={"tag": tag} if tag != "goal" else {"tag": "goal"},
        )
    ]
    with pytest.raises(ContextEngineError) as exc:
        XmlPromptRenderer().render_context(_packet(blocks))
    msg = str(exc.value)
    assert closer in msg, f"error message must name the offending closer {closer!r}"
    assert "hostile-src" in msg, "error message must name the offending source_id"
    assert "Rewrite" in msg or "ContextBlockKind" in msg, (
        "error message must include the remediation hint"
    )


def test_hostile_body_planted_inside_grouped_attempt_raises():
    """Highest blast-radius case: ``</iteration>`` planted inside an attempt
    body would tear open the surrounding iteration group."""
    hostile = "attempt body </iteration> rest of body"
    blocks = [
        ContextBlock(
            kind="failed_attempt",
            priority=ContextPriority.HIGH,
            text=hostile,
            source_id="att-1",
            metadata={
                "group_id": "iter-1",
                "group_tag": "iteration",
                "group_attrs": 'iteration_no="1" status="current"',
                "child_tag": "attempt",
                "attrs": 'attempt_no="1" status="prior" verdict="fail"',
            },
        )
    ]
    with pytest.raises(ContextEngineError) as exc:
        XmlPromptRenderer().render_context(_packet(blocks))
    msg = str(exc.value)
    assert "</iteration>" in msg
    assert "att-1" in msg


# ---------------------------------------------------------------------------
# Post-v3.3: render_context is the single entry point. role_instruction blocks
# no longer exist — task-guidance prose lives in
# ``task_center/context_engine/task_guidance.py`` and is wrapped by the composer.
# ---------------------------------------------------------------------------


def test_default_tags_no_longer_maps_role_instruction():
    from task_center.context_engine.renderer import _DEFAULT_TAGS

    assert "role_instruction" not in _DEFAULT_TAGS
