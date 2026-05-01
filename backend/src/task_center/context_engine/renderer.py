"""PromptRenderer — pure function over :class:`ContextPacket`.

The renderer never touches stores or runtime objects. It walks blocks in
priority order, groups by kind via a per-kind heading template, and respects
``packet.metadata['token_budget']`` when present.

Inherited blocks (``metadata['inherited_from_parent'] == 'true'``) are
segregated under a single ``# Parent context`` heading so helper agents see
their own contract first and the parent's frame underneath.
"""

from __future__ import annotations

from typing import Protocol

from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
)

_PRIORITY_ORDER: tuple[ContextPriority, ...] = (
    ContextPriority.REQUIRED,
    ContextPriority.HIGH,
    ContextPriority.MEDIUM,
    ContextPriority.LOW,
)

_INHERITED_FLAG = "inherited_from_parent"
_TOKEN_BUDGET_KEY = "token_budget"

# Approximate, deterministic token estimate (4 chars ≈ 1 token). Used for
# compression decisions; the renderer does not call out to a tokenizer.
_CHARS_PER_TOKEN = 4


class PromptRenderer(Protocol):
    """Renders a :class:`ContextPacket` to a single ``task_input`` string."""

    def render(self, packet: ContextPacket) -> str: ...


def _humanize(kind: str) -> str:
    return kind.replace("_", " ").strip().capitalize()


class HeadingTemplate:
    """Per-kind heading template registry.

    Templates are plain ``str.format`` strings receiving the block kind and
    optional metadata. Adding a kind is a config-only change.
    """

    def __init__(self, defaults: dict[str, str] | None = None) -> None:
        self._templates: dict[str, str] = dict(defaults or {})

    def register(self, kind: str, template: str) -> None:
        self._templates[kind] = template

    def heading_for(self, block: ContextBlock) -> str:
        template = self._templates.get(block.kind, "# {title}")
        try:
            return template.format(
                kind=block.kind,
                title=_humanize(block.kind),
                **block.metadata,
            )
        except KeyError:
            # Missing metadata key — fall back to the simple title heading so a
            # block that travels without its labeling metadata still renders
            # cleanly (e.g. inherited blocks from a parent packet).
            return f"# {_humanize(block.kind)}"


def default_heading_template() -> HeadingTemplate:
    """Templates that label per-kind blocks with metadata-driven suffixes."""
    return HeadingTemplate(
        defaults={
            "complex_task_goal": "# Complex task goal",
            "segment_goal": "# Segment goal",
            "prior_segment_specification": (
                "# Prior segment specification (segment {segment_sequence_no})"
            ),
            "prior_segment_summary": (
                "# Prior segment summary (segment {segment_sequence_no})"
            ),
            "failed_graph_landscape": (
                "# Failed graph landscape (attempt {graph_sequence_no})"
            ),
            "planned_task_spec": "# Your task spec",
            "task_specification": "# Task specification",
            "evaluation_criteria": "# Evaluation criteria",
            "dependency_summary": "# Dependency summary ({dep_id})",
            "completed_task_summary": "# Completed task summary ({task_id})",
            "artifact_reference": "# Artifact reference",
            "entry_request": "# Entry request",
            "parent_question": "# Parent question",
            "capability_note": "# Capability note",
        }
    )


def _estimate_tokens(text: str) -> int:
    return max(1, (len(text) + _CHARS_PER_TOKEN - 1) // _CHARS_PER_TOKEN)


def _is_inherited(block: ContextBlock) -> bool:
    return block.metadata.get(_INHERITED_FLAG) == "true"


class MarkdownPromptRenderer:
    """Default markdown renderer.

    Implements the compression policy from plan §3.4 / phase-06 §"Token and
    compression policy":

    1. Always include ``required`` blocks verbatim.
    2. Include ``high`` blocks; if over budget, truncate longest ``low`` first,
       then ``medium``, never ``required`` / ``high``.
    3. Replace truncated bodies with a one-line evidence reference if
       ``source_id`` is set; otherwise an ellipsis marker.
    """

    def __init__(
        self,
        heading_template: HeadingTemplate | None = None,
    ) -> None:
        self._headings = heading_template or default_heading_template()

    def render(self, packet: ContextPacket) -> str:
        budget = self._budget_from(packet)
        kept_blocks = self._compress(packet.blocks, budget=budget)
        helper_owned, inherited = self._split_inherited(kept_blocks)
        sections: list[str] = []
        sections.extend(self._render_blocks(helper_owned, packet=packet))
        if inherited:
            sections.append("# Parent context")
            sections.extend(self._render_blocks(inherited, packet=packet))
        return "\n\n".join(s for s in sections if s).strip() + "\n"

    # ---- internals ------------------------------------------------------

    @staticmethod
    def _budget_from(packet: ContextPacket) -> int | None:
        raw = packet.metadata.get(_TOKEN_BUDGET_KEY)
        if not raw:
            return None
        try:
            value = int(raw)
            return value if value > 0 else None
        except ValueError:
            return None

    @staticmethod
    def _split_inherited(
        blocks: list[ContextBlock],
    ) -> tuple[list[ContextBlock], list[ContextBlock]]:
        owned = [b for b in blocks if not _is_inherited(b)]
        inherited = [b for b in blocks if _is_inherited(b)]
        return owned, inherited

    def _render_blocks(
        self, blocks: list[ContextBlock], *, packet: ContextPacket
    ) -> list[str]:
        ordered = self._order_by_priority(blocks)
        out: list[str] = []
        for block in ordered:
            heading = self._headings.heading_for(block)
            subtitle = self._subtitle_for(block, packet=packet)
            body = block.text.strip()
            section = heading
            if subtitle:
                section += f"\n{subtitle}"
            section += f"\n\n{body}"
            out.append(section)
        return out

    @staticmethod
    def _subtitle_for(block: ContextBlock, *, packet: ContextPacket) -> str | None:
        if (
            block.kind == "segment_goal"
            and packet.metadata.get("is_initial_segment") == "true"
        ):
            return "*(first segment — equal to the original request goal)*"
        return None

    @staticmethod
    def _order_by_priority(blocks: list[ContextBlock]) -> list[ContextBlock]:
        rank = {p: i for i, p in enumerate(_PRIORITY_ORDER)}
        # Stable sort preserves insertion order within a priority bucket.
        return sorted(blocks, key=lambda b: rank.get(b.priority, len(rank)))

    def _compress(
        self,
        blocks: list[ContextBlock],
        *,
        budget: int | None,
    ) -> list[ContextBlock]:
        if budget is None:
            return list(blocks)

        ordered = self._order_by_priority(blocks)
        running = sum(_estimate_tokens(b.text) for b in ordered)
        if running <= budget:
            return ordered

        # Drop / truncate priority by priority, longest first within priority.
        for drop_priority in (
            ContextPriority.LOW,
            ContextPriority.MEDIUM,
        ):
            if running <= budget:
                break
            droppable = [
                (idx, b)
                for idx, b in enumerate(ordered)
                if b.priority == drop_priority
            ]
            droppable.sort(key=lambda pair: -_estimate_tokens(pair[1].text))
            for idx, block in droppable:
                if running <= budget:
                    break
                replacement = self._truncate(block)
                running -= _estimate_tokens(block.text)
                running += _estimate_tokens(replacement.text)
                ordered[idx] = replacement
        return ordered

    @staticmethod
    def _truncate(block: ContextBlock) -> ContextBlock:
        if block.source_id:
            text = (
                f"({block.kind}: see source {block.source_id} — "
                f"truncated for token budget)"
            )
        else:
            text = f"({block.kind}: … truncated for token budget)"
        return block.model_copy(update={"text": text})
