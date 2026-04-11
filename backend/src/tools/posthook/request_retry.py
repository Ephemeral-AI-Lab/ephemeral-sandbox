"""``request_retry`` tool — signals that the current work item should be retried."""

from __future__ import annotations

from typing import Any

from pydantic import BaseModel, Field

from team.models import RetryRequest
from tools.core.base import ToolExecutionContext
from tools.posthook.base import SubmitPosthookTool


class RequestRetryInput(BaseModel):
    reason: str = Field(
        ...,
        description="Why this work item should be retried, including the transient phase or runtime symptom when known.",
        min_length=1,
    )


class RequestRetryTool(SubmitPosthookTool):
    name: str = "request_retry"
    description: str = (
        "Request that the current work item be retried. Use when the failure "
        "is transient (e.g. flaky test, timeout, model confusion) and the same "
        "task is likely to succeed on re-execution; include the concrete transient symptom in `reason`."
    )
    input_model = RequestRetryInput
    default_metadata_key: str = "submitted_summary"

    def _build_payload(
        self, arguments: BaseModel, context: ToolExecutionContext
    ) -> tuple[Any, str | None]:
        assert isinstance(arguments, RequestRetryInput)
        retry_count = int(context.metadata.get("retry_count") or 0)
        max_retries = int(context.metadata.get("max_retries") or 2)
        if retry_count >= max_retries:
            return None, (
                f"Retry budget exhausted ({retry_count}/{max_retries}). "
                f"Consider using request_replan to escalate if available, "
                f"or call submit_summary with a failure summary."
            )
        return (
            RetryRequest(
                reason=arguments.reason,
                retry_count=retry_count,
                max_retries=max_retries,
            ),
            None,
        )

    def _accepted_message(self, payload: Any) -> str:
        assert isinstance(payload, RetryRequest)
        return (
            f"Retry request accepted (attempt {payload.retry_count + 1}"
            f"/{payload.max_retries})."
        )
