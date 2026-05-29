"""Canonical tag → label dictionary for ``render_context_outline``.

Each :class:`TagDescriptor` pairs a structural tag (and optional
*semantic-attribute filter*) with the prose label the ``What's in context``
outline uses. The dictionary is the single source of truth for those labels:
two cases that reference the same ``(tag, semantic-attribute-set)`` produce
byte-identical bullets.

Only ``status``, ``verdict``, and ``position`` are *semantic* attributes —
they change what a tag means. ``iteration_no``, ``attempt_no``, ``task_id``,
``id`` are *identity* attributes — they distinguish instances but never change
the label. The dictionary keys on semantic attributes only.

Wiring contract:

* ``match(tag, attrs)`` returns the most-specific matching descriptor (an
  entry with ``attr_filter`` set wins over a wildcard entry on the same tag).
* :data:`RECURSE_THROUGH` names the tags whose children get nested bullets.
  ``<iteration>`` is the only entry today.
"""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict


class TagDescriptor(BaseModel):
    """One row in the canonical tag dictionary."""

    tag: str
    attr_filter: dict[str, str] | None = None
    label: str

    model_config = ConfigDict(frozen=True, extra="forbid")


TAG_DICTIONARY: list[TagDescriptor] = [
    TagDescriptor(tag="goal", attr_filter=None, label="user's request"),
    TagDescriptor(
        tag="entry_request",
        attr_filter=None,
        label="root delegation envelope",
    ),
    TagDescriptor(
        tag="iteration",
        attr_filter={"position": "prior"},
        label="previous iteration's work",
    ),
    TagDescriptor(
        tag="iteration",
        attr_filter={"position": "current"},
        label="active iteration",
    ),
    TagDescriptor(
        tag="iteration_goal",
        attr_filter=None,
        label="active iteration's scope",
    ),
    TagDescriptor(
        tag="attempt",
        attr_filter=None,
        label="failed prior attempt",
    ),
    TagDescriptor(tag="plan_spec", attr_filter=None, label="attempt's plan"),
    TagDescriptor(
        tag="deferred_goal_for_next_iteration",
        attr_filter=None,
        label="scope handed to next iteration",
    ),
    TagDescriptor(tag="task", attr_filter=None, label="generator task outcome"),
    TagDescriptor(
        tag="evaluation_criteria",
        attr_filter=None,
        label="criteria the attempt must satisfy",
    ),
    TagDescriptor(
        tag="evaluator_summary",
        attr_filter=None,
        label="evaluator's commentary",
    ),
    TagDescriptor(
        tag="assigned_task",
        attr_filter=None,
        label="your assigned task",
    ),
    TagDescriptor(
        tag="dependency",
        attr_filter=None,
        label="upstream task output",
    ),
]


RECURSE_THROUGH: frozenset[str] = frozenset({"iteration"})


# Identity attributes never surfaced in the outline.
_IDENTITY_ATTRS: frozenset[str] = frozenset({"iteration_no", "attempt_no", "task_id", "id"})

# Semantic attributes — the only ones the dictionary keys on.
_SEMANTIC_ATTRS: frozenset[str] = frozenset({"status", "verdict", "position"})


def match(tag: str, attrs: dict[str, str]) -> TagDescriptor | None:
    """Return the most-specific :class:`TagDescriptor` for ``(tag, attrs)``.

    A descriptor with ``attr_filter`` set wins over a wildcard entry on the
    same tag, provided every filter key matches the value in ``attrs``. Only
    ``status`` and ``verdict`` participate in matching — identity attributes
    are ignored.
    """
    semantic = {k: v for k, v in attrs.items() if k in _SEMANTIC_ATTRS}
    best: TagDescriptor | None = None
    best_specificity = -1
    for descriptor in TAG_DICTIONARY:
        if descriptor.tag != tag:
            continue
        if descriptor.attr_filter is None:
            specificity = 0
            matches = True
        else:
            specificity = len(descriptor.attr_filter)
            matches = all(semantic.get(k) == v for k, v in descriptor.attr_filter.items())
        if matches and specificity > best_specificity:
            best = descriptor
            best_specificity = specificity
    return best


def render_attrs(attrs: dict[str, str]) -> str:
    """Format ``attrs`` for an outline bullet's opening tag.

    Identity attributes (``iteration_no``, ``attempt_no``, ``task_id``, ``id``)
    are dropped; semantic attributes (``status``, ``verdict``, ``position``) are
    emitted in a stable order so two cases at the same ``(tag, semantic-set)``
    produce byte-identical bullets.
    """
    ordered: list[tuple[str, str]] = []
    for key in ("status", "verdict", "position"):
        if key in attrs:
            ordered.append((key, attrs[key]))
    for key, value in attrs.items():
        if key in _IDENTITY_ATTRS or key in _SEMANTIC_ATTRS:
            continue
        ordered.append((key, value))
    return " ".join(f'{k}="{v}"' for k, v in ordered)


__all__ = [
    "RECURSE_THROUGH",
    "TAG_DICTIONARY",
    "TagDescriptor",
    "match",
    "render_attrs",
]
