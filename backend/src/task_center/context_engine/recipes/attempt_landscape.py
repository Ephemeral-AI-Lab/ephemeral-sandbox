"""Failed attempt landscape blocks for planner context."""

from __future__ import annotations

from task_center.context_engine.packet import (
    ContextBlock,
    ContextBlockKind,
    ContextPriority,
)
from task_center.attempt import HarnessGraph, HarnessGraphStatus

MAX_FAILED_ATTEMPTS_RENDERED = 6


def failed_attempt_landscape_blocks(
    *,
    current_graph_id: str | None,
    graphs: list[HarnessGraph],
) -> list[ContextBlock]:
    failed = sorted(
        (
            g
            for g in graphs
            if g.status == HarnessGraphStatus.FAILED
            and g.id != current_graph_id
        ),
        key=lambda g: g.graph_sequence_no,
    )
    if not failed:
        return []

    if len(failed) <= MAX_FAILED_ATTEMPTS_RENDERED:
        rendered = failed
        truncated: list[HarnessGraph] = []
    else:
        rendered = failed[-MAX_FAILED_ATTEMPTS_RENDERED:]
        truncated = failed[:-MAX_FAILED_ATTEMPTS_RENDERED]

    blocks: list[ContextBlock] = [
        ContextBlock(
            kind=ContextBlockKind.FAILED_ATTEMPT_LANDSCAPE,
            priority=ContextPriority.HIGH,
            text=_render_failed_attempt(g),
            source_id=g.id,
            source_kind="harness_graph",
            metadata={
                "graph_sequence_no": str(g.graph_sequence_no),
                "group_heading": "# Failed Attempts",
                "subheading": f"Attempt {g.graph_sequence_no}",
            },
        )
        for g in rendered
    ]

    if truncated:
        blocks.append(
            ContextBlock(
                kind=ContextBlockKind.FAILED_ATTEMPT_LANDSCAPE,
                priority=ContextPriority.MEDIUM,
                text=(
                    f"{len(truncated)} earlier failed attempts omitted "
                    f"(graph_sequence_no "
                    f"{truncated[0].graph_sequence_no}-"
                    f"{truncated[-1].graph_sequence_no}). "
                    f"Most recent {MAX_FAILED_ATTEMPTS_RENDERED} attempts "
                    f"shown above."
                ),
                source_id=None,
                source_kind=None,
                metadata={
                    "truncated_count": str(len(truncated)),
                    "group_heading": "# Failed Attempts",
                    "subheading": "Earlier attempts omitted",
                },
            )
        )
    return blocks


def _render_failed_attempt(graph: HarnessGraph) -> str:
    criteria_block = (
        "\n".join(f"  - {c}" for c in graph.evaluation_criteria) or "  (none)"
    )
    return (
        f"task_specification: {graph.task_specification or '(missing)'}\n"
        f"evaluation_criteria:\n{criteria_block}\n"
        f"fail_reason: {graph.fail_reason.value if graph.fail_reason else 'unknown'}"
    )


__all__ = [
    "MAX_FAILED_ATTEMPTS_RENDERED",
    "failed_attempt_landscape_blocks",
]
