"""Posthook submit tools.

A posthook submit tool is the single exit point of a serializer agent
run by ``hooks.agent_posthook.execute_with_posthook``. It validates the
work-phase output and stashes the validated payload in
``context.metadata`` under the slot named by ``posthook_metadata_key``.

``SubmitPosthookTool`` is the abstract base; concrete tools like
``SubmitPlanTool`` and ``SubmitSummaryTool`` implement domain-specific
validation in ``_build_payload``.
"""

from tools.posthook.base import SubmitPosthookTool
from tools.posthook.submit_plan import SubmitPlanInput, SubmitPlanTool
from tools.posthook.submit_summary import (
    SubmittedSummary,
    SubmitSummaryInput,
    SubmitSummaryTool,
)

__all__ = [
    "SubmitPosthookTool",
    "SubmitPlanInput",
    "SubmitPlanTool",
    "SubmittedSummary",
    "SubmitSummaryInput",
    "SubmitSummaryTool",
]
