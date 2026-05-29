"""TAG_DICTIONARY canonical-label coverage + matching semantics.

Pins the registry against ``OPTIMIZED_USER_MSG_1.md``: every spec'd
``(tag, semantic-attrs)`` row maps to the spec's canonical label and only
``status`` / ``verdict`` / ``position`` participate in matching.
"""

from __future__ import annotations

import pytest

from task_center.context_engine.tag_dictionary import (
    RECURSE_THROUGH,
    TAG_DICTIONARY,
    match,
    render_attrs,
)


def test_dictionary_has_every_canonical_row_from_spec():
    expected: list[tuple[str, dict[str, str] | None, str]] = [
        ("goal", None, "user's request"),
        ("entry_request", None, "root delegation envelope"),
        ("iteration", {"position": "prior"}, "previous iteration's work"),
        ("iteration", {"position": "current"}, "active iteration"),
        ("iteration_goal", None, "active iteration's scope"),
        # The attempt row is now a single wildcard; the prior/current and
        # status/verdict attempt rows were removed (contract §5). The
        # accepted_plan/summary/status_summary/failed_criteria rows were removed
        # too — the redesign no longer emits any of those elements.
        ("attempt", None, "failed prior attempt"),
        ("plan_spec", None, "attempt's plan"),
        (
            "deferred_goal_for_next_iteration",
            None,
            "scope handed to next iteration",
        ),
        ("task", None, "generator task outcome"),
        (
            "evaluation_criteria",
            None,
            "criteria the attempt must satisfy",
        ),
        ("evaluator_summary", None, "evaluator's commentary"),
        ("assigned_task", None, "your assigned task"),
        ("dependency", None, "upstream task output"),
    ]
    actual = [(d.tag, d.attr_filter, d.label) for d in TAG_DICTIONARY]
    assert actual == expected


def test_only_iteration_is_in_recurse_through():
    assert RECURSE_THROUGH == frozenset({"iteration"})


def test_match_picks_correct_iteration_position():
    # The attempt specific/wildcard pair was removed (contract §5), so the only
    # tag with multiple semantic-filter rows is <iteration>, keyed on position.
    prior = match("iteration", {"position": "prior"})
    current = match("iteration", {"position": "current"})
    assert prior is not None and prior.label == "previous iteration's work"
    assert current is not None and current.label == "active iteration"


def test_match_returns_none_for_unknown_tag():
    assert match("nonexistent_tag", {}) is None


def test_match_ignores_identity_attrs():
    desc = match("iteration", {"position": "current", "iteration_no": "7"})
    assert desc is not None and desc.label == "active iteration"


# Removed test_match_picks_more_specific_filter: its only exemplar of a 2-key
# filter winning over a 1-key filter was the <attempt status verdict> row, which
# was collapsed to a single wildcard row (contract §5). No multi-key filter
# remains in the live dictionary, so 2-key>1-key ranking is no longer testable.


def test_render_attrs_orders_semantic_first_and_drops_identity():
    out = render_attrs(
        {
            "iteration_no": "1",
            "verdict": "fail",
            "status": "prior",
            "position": "current",
            "id": "x",
        }
    )
    # position is a semantic attr too (contract §3); it renders after verdict.
    assert out == 'status="prior" verdict="fail" position="current"'


def test_render_attrs_empty_for_only_identity_attrs():
    assert render_attrs({"iteration_no": "1", "id": "x"}) == ""


def test_descriptor_is_frozen():
    desc = TAG_DICTIONARY[0]
    with pytest.raises(Exception):
        desc.tag = "renamed"  # type: ignore[misc]
