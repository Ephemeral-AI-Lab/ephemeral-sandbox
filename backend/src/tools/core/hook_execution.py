"""Sequential pre/post hook execution for tools."""

from __future__ import annotations

import json
from collections.abc import Awaitable, Callable
from dataclasses import replace
from typing import Any

from pydantic import BaseModel, ValidationError

from message.stream_events import StreamEvent
from notification.notifications import (
    SYSTEM_NOTIFICATIONS_METADATA_KEY,
    serialize_system_notifications,
)
from notification._runtime import SystemNotificationService
from tools.core.base import BaseTool
from tools.core.context import ToolExecutionContextService
from tools.core.hooks import HookResult, hook_name, validate_hook_targets
from tools.core.results import ToolResult
from tools.core.validation import validate_tool_output


EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]
_HookTraceEntry = dict[str, object]


class ToolHookExecutionHelper:
    """Runs tool-specific hooks and owns their metadata/notification plumbing."""

    def __init__(
        self,
        tool: BaseTool,
        context: ToolExecutionContextService,
        emit: EmitStreamEvent,
    ) -> None:
        validate_hook_targets(
            tool_name=tool.name,
            pre_hooks=tuple(getattr(tool, "pre_hooks", ()) or ()),
            post_hooks=tuple(getattr(tool, "post_hooks", ()) or ()),
        )
        self._tool = tool
        self._context = context
        self._system_notification_service = self._ensure_notification_service(
            context,
            emit,
        )
        self._hook_trace: list[_HookTraceEntry] = []

    async def run_pre_hooks(
        self,
        parsed_input: BaseModel,
    ) -> tuple[BaseModel | None, ToolResult | None]:
        current = parsed_input
        for hook in tuple(getattr(self._tool, "pre_hooks", ()) or ()):
            try:
                outcome = await hook.run(current, self._context)
            except Exception as exc:
                reason = f"{hook_name(hook)} raised {exc.__class__.__name__}: {exc}"
                return None, self._build_hook_failure_result(
                    phase="pre",
                    hook=hook,
                    reason=reason,
                    effective_input=current,
                )
            invalid_outcome = self._invalid_hook_result(hook, outcome)
            if invalid_outcome is not None:
                return None, self._build_hook_failure_result(
                    phase="pre",
                    hook=hook,
                    reason=invalid_outcome,
                    effective_input=current,
                )
            assert isinstance(outcome, HookResult)
            if outcome.status == "fail":
                reason = outcome.reason or outcome.message or f"{hook_name(hook)} denied execution."
                return None, self._build_hook_failure_result(
                    phase="pre",
                    hook=hook,
                    reason=reason,
                    message=outcome.message,
                    metadata=outcome.metadata,
                    effective_input=current,
                )
            next_value = current if outcome.value is None else outcome.value
            try:
                current = self._validated_hook_input(next_value)
            except ValidationError as exc:
                reason = (
                    f"{hook_name(hook)} returned input inconsistent with "
                    f"{self._tool.input_model.__name__}: {self._format_validation_errors(exc)}."
                )
                return None, self._build_hook_failure_result(
                    phase="pre",
                    hook=hook,
                    reason=reason,
                    message=outcome.message,
                    metadata=outcome.metadata,
                    effective_input=current,
                )
            self._append_trace(
                phase="pre",
                hook=hook,
                status="pass",
                message=outcome.message,
                metadata=outcome.metadata,
            )
        return current, None

    async def run_post_hooks(
        self,
        parsed_input: BaseModel,
        result: ToolResult,
    ) -> ToolResult:
        current = result
        for hook in tuple(getattr(self._tool, "post_hooks", ()) or ()):
            try:
                outcome = await hook.run(parsed_input, current, self._context)
            except Exception as exc:
                reason = f"{hook_name(hook)} raised {exc.__class__.__name__}: {exc}"
                return self._build_hook_failure_result(
                    phase="post",
                    hook=hook,
                    reason=reason,
                    effective_input=parsed_input,
                )
            invalid_outcome = self._invalid_hook_result(hook, outcome)
            if invalid_outcome is not None:
                return self._build_hook_failure_result(
                    phase="post",
                    hook=hook,
                    reason=invalid_outcome,
                    effective_input=parsed_input,
                )
            assert isinstance(outcome, HookResult)
            if outcome.status == "fail":
                reason = outcome.reason or outcome.message or f"{hook_name(hook)} denied result."
                return self._build_hook_failure_result(
                    phase="post",
                    hook=hook,
                    reason=reason,
                    message=outcome.message,
                    metadata=outcome.metadata,
                    effective_input=parsed_input,
                )
            next_value = current if outcome.value is None else outcome.value
            if not isinstance(next_value, ToolResult):
                reason = (
                    f"{hook_name(hook)} returned {type(next_value).__name__}; "
                    "expected ToolResult."
                )
                return self._build_hook_failure_result(
                    phase="post",
                    hook=hook,
                    reason=reason,
                    message=outcome.message,
                    metadata=outcome.metadata,
                    effective_input=parsed_input,
                )
            validation_error = self._validate_hook_output(next_value)
            if validation_error is not None:
                reason = (
                    f"{hook_name(hook)} returned output inconsistent with "
                    f"{self._tool.output_model.__name__}: {validation_error}"
                )
                return self._build_hook_failure_result(
                    phase="post",
                    hook=hook,
                    reason=reason,
                    message=outcome.message,
                    metadata=outcome.metadata,
                    effective_input=parsed_input,
                )
            current = next_value
            self._append_trace(
                phase="post",
                hook=hook,
                status="pass",
                message=outcome.message,
                metadata=outcome.metadata,
            )
        return current

    def finalize_result(self, result: ToolResult, *, effective_input: BaseModel) -> ToolResult:
        if "hook_failure" in result.metadata:
            return result
        return self._with_hook_details(result, effective_input=effective_input)

    @staticmethod
    def _ensure_notification_service(
        context: ToolExecutionContextService,
        emit: EmitStreamEvent,
    ) -> SystemNotificationService:
        existing = context.get("system_notification_service")
        if isinstance(existing, SystemNotificationService):
            if not existing.has_registered_agent_run and existing.emit is None:
                existing.emit = emit
            return existing
        service = SystemNotificationService(emit=emit)
        context.update_services(system_notification_service=service)
        return service

    @staticmethod
    def _hook_event_name(phase: str) -> str:
        return "PreToolUse" if phase == "pre" else "PostToolUse"

    @staticmethod
    def _format_validation_errors(exc: ValidationError) -> str:
        return "; ".join(
            f"{'.'.join(str(p) for p in e['loc'])}: {e['msg']}" for e in exc.errors()
        )

    @staticmethod
    def _invalid_hook_result(hook: object, outcome: object) -> str | None:
        if isinstance(outcome, HookResult):
            return None
        return f"{hook_name(hook)} returned {type(outcome).__name__}; expected HookResult."

    def _append_trace(
        self,
        *,
        phase: str,
        hook: object,
        status: str,
        reason: str = "",
        message: str = "",
        metadata: dict[str, object] | None = None,
    ) -> None:
        self._hook_trace.append(
            {
                "phase": phase,
                "hook_name": hook_name(hook),
                "status": status,
                "reason": reason,
                "message": message,
                "metadata": dict(metadata or {}),
            }
        )

    def _metadata_with_hook_details(
        self,
        result: ToolResult,
        *,
        effective_input: BaseModel | None,
    ) -> dict[str, Any]:
        metadata = dict(result.metadata or {})
        if self._hook_trace:
            existing_trace = metadata.get("hook_trace")
            if isinstance(existing_trace, list):
                metadata["hook_trace"] = [*existing_trace, *self._hook_trace]
            else:
                metadata["hook_trace"] = self._hook_trace
        if effective_input is not None and self._hook_trace:
            metadata["effective_tool_input"] = effective_input.model_dump(mode="json")
        if not self._system_notification_service.has_registered_agent_run:
            notifications = self._system_notification_service.pop_pending_notifications()
            if notifications:
                metadata[SYSTEM_NOTIFICATIONS_METADATA_KEY] = (
                    serialize_system_notifications(notifications)
                )
        return metadata

    def _with_hook_details(
        self,
        result: ToolResult,
        *,
        effective_input: BaseModel | None,
    ) -> ToolResult:
        metadata = self._metadata_with_hook_details(
            result,
            effective_input=effective_input,
        )
        if metadata == result.metadata:
            return result
        return replace(result, metadata=metadata)

    def _build_hook_failure_result(
        self,
        *,
        phase: str,
        hook: object,
        reason: str,
        effective_input: BaseModel | None,
        message: str = "",
        metadata: dict[str, object] | None = None,
    ) -> ToolResult:
        self._append_trace(
            phase=phase,
            hook=hook,
            status="fail",
            reason=reason,
            message=message,
            metadata=metadata,
        )
        hook_failure = {
            "phase": phase,
            "hook_name": hook_name(hook),
            "tool_name": self._tool.name,
            "reason": reason,
            "hook_event_name": self._hook_event_name(phase),
            "permission_decision": "deny",
            "permission_decision_reason": reason,
        }
        output = json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": hook_failure["hook_event_name"],
                    "permissionDecision": hook_failure["permission_decision"],
                    "permissionDecisionReason": reason,
                },
                "hookName": hook_failure["hook_name"],
                "toolName": self._tool.name,
                "phase": phase,
            },
            indent=2,
        )
        result = ToolResult(
            output=output,
            is_error=True,
            metadata={"hook_failure": hook_failure},
        )
        return self._with_hook_details(result, effective_input=effective_input)

    def _validated_hook_input(self, value: object) -> BaseModel:
        raw = value.model_dump(mode="json") if isinstance(value, BaseModel) else value
        return self._tool.input_model.model_validate(raw)

    def _validate_hook_output(self, result: ToolResult) -> str | None:
        if result.is_error:
            return None
        validated = validate_tool_output(self._tool, result)
        if validated.is_error:
            return validated.output
        return None
