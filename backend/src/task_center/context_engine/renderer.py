"""PromptRenderer — pure function over :class:`ContextPacket`.

The renderer never touches stores or runtime objects. It walks blocks in packet
order, wraps each block body verbatim in its XML tag, and respects
``packet.metadata['token_budget']`` when present. Priority is a compression
policy, not a presentation-order policy.

**Verbatim contract.** The renderer never modifies ``block.text``. No
``.strip()``, no whitespace normalization, no newline reflow. Recipes are
responsible for the byte-exact body they want emitted.

**Hostile-body guard.** Before emitting, the renderer scans ``block.text`` for
any structural tag-closer the packet itself emits (e.g. ``</goal>``,
``</attempt_plan>``). A match raises :class:`ContextEngineError` with the
offending closer, the block's ``source_id``, and a remediation hint — there is
no silent escaping. Rewrite the block body or use a different
:class:`ContextBlockKind` for the offending content.
"""

from __future__ import annotations

from task_center.context_engine.exceptions import ContextEngineError
from task_center.context_engine.packet import (
    ContextBlock,
    ContextPacket,
    ContextPriority,
)

_TOKEN_BUDGET_KEY = "token_budget"

# Approximate, deterministic token estimate (4 chars ≈ 1 token). Used for
# compression decisions; the renderer does not call out to a tokenizer.
_CHARS_PER_TOKEN = 4

# Default tag name for each :class:`ContextBlockKind`. Recipes may override per
# block via ``metadata['tag']`` (standalone) or ``metadata['child_tag']``
# (inside a group); the kind→tag mapping is the fallback.
_DEFAULT_TAGS: dict[str, str] = {
    "goal_statement": "goal",
    "iteration_statement": "iteration_goal",
    "prior_iteration_summary": "summary",
    "failed_attempt": "attempt",
    "planned_task_spec": "assigned_task",
    "task_specification": "plan_spec",
    "dependency_summary": "dependency",
    "entry_request": "entry_request",
}


def _estimate_tokens(text: str) -> int:
    return max(1, (len(text) + _CHARS_PER_TOKEN - 1) // _CHARS_PER_TOKEN)


class XmlPromptRenderer:
    """XML-tagged renderer.

    Implements the compression policy from plan §3.4 / phase-06 §"Token and
    compression policy":

    1. Always include ``required`` blocks verbatim.
    2. Include ``high`` blocks; if over budget, truncate longest ``low`` first,
       then ``medium``, never ``required`` / ``high``.
    3. Replace truncated bodies with a one-line evidence reference if
       ``source_id`` is set; otherwise an ellipsis marker.
    """

    def render_context(self, packet: ContextPacket) -> str:
        """Render world-state context."""
        kept_blocks = self._compress(list(packet.blocks), budget=self._budget_from(packet))
        self._validate_no_structural_closers(kept_blocks)
        sections = self._render_blocks(kept_blocks)
        body = "\n\n".join(s for s in sections if s)
        return body + "\n" if body else ""

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

    def _render_blocks(self, blocks: list[ContextBlock]) -> list[str]:
        out: list[str] = []
        index = 0
        while index < len(blocks):
            block = blocks[index]
            group_id = block.metadata.get("group_id")
            if group_id:
                group: list[ContextBlock] = []
                while index < len(blocks) and blocks[index].metadata.get("group_id") == group_id:
                    group.append(blocks[index])
                    index += 1
                out.append(self._render_group(group))
                continue
            out.append(self._render_block(block))
            index += 1
        return out

    def _render_block(self, block: ContextBlock) -> str:
        tag = self._tag_for(block)
        attrs = block.metadata.get("attrs", "")
        return f"<{tag}{_attrs_suffix(attrs)}>\n{block.text}\n</{tag}>"

    def _render_group(self, blocks: list[ContextBlock]) -> str:
        first = blocks[0]
        group_tag = first.metadata.get("group_tag")
        if not group_tag:
            raise ContextEngineError(
                "Grouped blocks must set metadata['group_tag'] on the first "
                f"member (group_id={first.metadata.get('group_id')!r})"
            )
        group_attrs = first.metadata.get("group_attrs", "")
        children: list[str] = []
        for block in blocks:
            child_tag = block.metadata.get("child_tag") or _DEFAULT_TAGS.get(block.kind)
            if child_tag is None:
                raise ContextEngineError(
                    f"No tag mapping for kind {block.kind!r} (grouped under {group_tag!r})"
                )
            child_attrs = block.metadata.get("attrs", "")
            children.append(
                f"<{child_tag}{_attrs_suffix(child_attrs)}>\n{block.text}\n</{child_tag}>"
            )
        return (
            f"<{group_tag}{_attrs_suffix(group_attrs)}>\n"
            + "\n".join(children)
            + f"\n</{group_tag}>"
        )

    def _tag_for(self, block: ContextBlock) -> str:
        tag = block.metadata.get("tag") or _DEFAULT_TAGS.get(block.kind)
        if tag is None:
            raise ContextEngineError(f"No tag mapping for kind {block.kind!r}")
        return tag

    def _validate_no_structural_closers(self, blocks: list[ContextBlock]) -> None:
        """Reject any block whose text contains a structural tag-closer.

        Computes the set of closers the renderer could emit for this packet
        (from ``_DEFAULT_TAGS`` plus per-block ``tag``/``child_tag``/``group_tag``
        overrides). If a body contains one of those closers, the surrounding
        XML envelope would be torn open mid-block.

        A block opts out of this check by setting
        ``metadata['pre_rendered_xml'] = 'true'``. Recipes that hand-assemble
        nested XML inside ``block.text`` (the ``attempts`` recipe) own the
        responsibility for sanitizing the user-supplied fragments they embed
        — the renderer trusts the recipe-controlled wrapper.
        """
        tags = set(_DEFAULT_TAGS.values())
        for block in blocks:
            for key in ("tag", "child_tag", "group_tag"):
                value = block.metadata.get(key)
                if value:
                    tags.add(value)
        closers = {f"</{t}>" for t in tags}
        for block in blocks:
            if block.metadata.get("pre_rendered_xml") == "true":
                continue
            for closer in closers:
                if closer in block.text:
                    raise ContextEngineError(
                        f"Block body contains structural closer {closer!r} "
                        f"(source_id={block.source_id!r}). "
                        "Rewrite the block body to avoid this structural "
                        "closer, or use a different ContextBlockKind for "
                        "this content."
                    )

    def _compress(
        self,
        blocks: list[ContextBlock],
        *,
        budget: int | None,
    ) -> list[ContextBlock]:
        """Apply the token-budget compression policy and return a fresh list.

        The input ``blocks`` list is never mutated: truncated entries are
        replaced via ``ContextBlock.model_copy`` (returns a new Pydantic model).
        """
        if budget is None:
            return list(blocks)

        kept = list(blocks)
        running = sum(_estimate_tokens(b.text) for b in kept)
        if running <= budget:
            return kept

        for drop_priority in (ContextPriority.LOW, ContextPriority.MEDIUM):
            if running <= budget:
                break
            droppable = sorted(
                ((idx, b) for idx, b in enumerate(kept) if b.priority == drop_priority),
                key=lambda pair: -_estimate_tokens(pair[1].text),
            )
            for idx, block in droppable:
                if running <= budget:
                    break
                replacement = _truncate(block)
                running += _estimate_tokens(replacement.text) - _estimate_tokens(block.text)
                kept[idx] = replacement
        return kept


def _attrs_suffix(attrs: str) -> str:
    """Format an attribute string for tag opening (leading space if non-empty)."""
    return f" {attrs}" if attrs else ""


def _truncate(block: ContextBlock) -> ContextBlock:
    if block.source_id:
        text = f"({block.kind}: see source {block.source_id} — truncated for token budget)"
    else:
        text = f"({block.kind}: … truncated for token budget)"
    return block.model_copy(update={"text": text})
