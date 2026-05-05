"""PromptRenderer — pure function over :class:`ContextPacket`.

The renderer never touches stores or runtime objects. It walks blocks in packet
order, applies per-block or per-kind headings, and respects
``packet.metadata['token_budget']`` when present. Priority is a compression
policy only; it is not a presentation-order policy.

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
        explicit = block.metadata.get("heading")
        if explicit:
            return explicit
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
            "complex_task_goal": "# Mission",
            "segment_goal": "# Current Episode",
            "prior_segment_specification": (
                "# Previous Episode Results"
            ),
            "prior_segment_summary": (
                "# Previous Episode Results"
            ),
            "failed_attempt_landscape": (
                "# Failed Attempts"
            ),
            "planned_task_spec": "# Assigned Task",
            "task_specification": "# Attempt Plan",
            "evaluation_criteria": "# Evaluation Criteria",
            "dependency_summary": "# Dependency Results",
            "completed_task_summary": "# Dependency Results",
            "artifact_reference": "# Artifact reference",
            "entry_request": "# Entry request",
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
        sections.extend(self._render_blocks(helper_owned))
        if inherited:
            sections.append("# Parent context")
            sections.extend(self._render_blocks(inherited))
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
        self, blocks: list[ContextBlock]
    ) -> list[str]:
        out: list[str] = []
        index = 0
        while index < len(blocks):
            block = blocks[index]
            group_heading = block.metadata.get("group_heading")
            if group_heading:
                group: list[ContextBlock] = []
                while (
                    index < len(blocks)
                    and blocks[index].metadata.get("group_heading")
                    == group_heading
                ):
                    group.append(blocks[index])
                    index += 1
                out.append(self._render_group(group_heading, group))
                continue

            out.append(self._render_block(block))
            index += 1
        return out

    def _render_block(self, block: ContextBlock) -> str:
        heading = self._headings.heading_for(block)
        subtitle = block.metadata.get("subtitle") or None
        body = block.text.strip()
        section = heading
        if subtitle:
            section += f"\n{subtitle}"
        section += f"\n\n{body}"
        return section

    def _render_group(
        self, heading: str, blocks: list[ContextBlock]
    ) -> str:
        parts = [heading]
        for block in blocks:
            subheading = block.metadata.get("subheading") or _humanize(block.kind)
            body = block.text.strip()
            parts.append(f"## {subheading}\n\n{body}")
        return "\n\n".join(parts)

    def _compress(
        self,
        blocks: list[ContextBlock],
        *,
        budget: int | None,
    ) -> list[ContextBlock]:
        if budget is None:
            return list(blocks)

        kept = list(blocks)
        running = sum(_estimate_tokens(b.text) for b in kept)
        if running <= budget:
            return kept

        # Drop / truncate priority by priority, longest first within priority.
        for drop_priority in (
            ContextPriority.LOW,
            ContextPriority.MEDIUM,
        ):
            if running <= budget:
                break
            droppable = [
                (idx, b)
                for idx, b in enumerate(kept)
                if b.priority == drop_priority
            ]
            droppable.sort(key=lambda pair: -_estimate_tokens(pair[1].text))
            for idx, block in droppable:
                if running <= budget:
                    break
                replacement = self._truncate(block)
                running -= _estimate_tokens(block.text)
                running += _estimate_tokens(replacement.text)
                kept[idx] = replacement
        return kept

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
