"""Single pipeline step executor — work agent + optional posthook agent."""

from __future__ import annotations

import json
import logging
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from agents import get_agent_definition
from engine.agent import spawn_agent
from message import ConversationMessage
from pipeline.models import StepRecord, StepStatus
from pipeline.schema import PipelineStepConfig, PipelineConfig

if TYPE_CHECKING:
    from server.app_factory import SessionConfig

logger = logging.getLogger(__name__)


@dataclass
class StepResult:
    """Output of a single step execution."""

    validated_output: dict[str, Any]
    record: StepRecord


class StepRunner:
    """Execute a single pipeline step: work agent -> optional posthook agent."""

    def __init__(
        self,
        step_config: PipelineStepConfig,
        pipeline_config: PipelineConfig,
        context_map: dict[str, dict[str, Any]],
        session_config: "SessionConfig",
    ) -> None:
        self._step = step_config
        self._pipeline = pipeline_config
        self._context_map = context_map
        self._session_config = session_config

    async def run(self, goal: str) -> StepResult:
        """Run work step, then optional posthook agent."""
        record = StepRecord(
            name=self._step.name,
            agent=self._step.agent,
            status=StepStatus.RUNNING,
            started_at=time.time(),
        )

        # 1. Resolve input deps
        step_inputs = self._resolve_input_deps()

        # 2. Build runtime context for the work agent
        runtime_context = self._build_work_context(goal, step_inputs)

        # 3. Run work agent
        work_response = await self._run_agent(
            agent_name=self._step.agent,
            prompt=runtime_context,
            toolkit_metadata={
                "pipeline_context_map": self._context_map,
                "pipeline_meta": {
                    "pipeline_id": self._pipeline.pipeline_id,
                    "pipeline_name": self._pipeline.name,
                    "goal": goal,
                    "step_name": self._step.name,
                    "step_config": self._step.config,
                },
                "pipeline_current_step": self._step.name,
            },
        )
        record.work_session_id = self._session_config.session_id
        record.metrics["response_text"] = work_response

        # 4. Posthook agent (if configured) — just another LLM run
        if self._step.posthook_agent:
            posthook_prompt = self._build_posthook_context(work_response)
            posthook_response = await self._run_agent(
                agent_name=self._step.posthook_agent,
                prompt=posthook_prompt,
                toolkit_metadata={},
            )
            record.posthook_session_id = self._session_config.session_id
            validated_output = self._parse_output(posthook_response)
        else:
            validated_output = self._parse_output(work_result.text)

        record.status = StepStatus.COMPLETED
        record.finished_at = time.time()

        return StepResult(validated_output=validated_output, record=record)

    def _resolve_input_deps(self) -> dict[str, Any]:
        """Build input context from declared input_deps."""
        inputs: dict[str, Any] = {}
        for dep in self._step.input_deps:
            step_output = self._context_map.get(dep.step, {})
            if dep.keys:
                inputs[dep.step] = {k: step_output[k] for k in dep.keys if k in step_output}
            else:
                inputs[dep.step] = step_output
        return inputs

    def _build_work_context(self, goal: str, step_inputs: dict[str, Any]) -> str:
        """Build runtime context injected into the work agent."""
        parts = [
            f"# Pipeline Step: {self._step.name}",
            f"\n## Goal\n{goal}",
        ]
        if self._step.description:
            parts.append(f"\n## Step Description\n{self._step.description}")

        if step_inputs:
            parts.append("\n## Prior Step Outputs")
            parts.append(
                "The following outputs from prior steps are available. "
                "You can also use the `query_pipeline_context` tool to query them."
            )
            for step_name, output in step_inputs.items():
                parts.append(f"\n### {step_name}")
                parts.append(f"```json\n{json.dumps(output, default=str, indent=2)}\n```")

        if self._step.output_schema:
            parts.append("\n## Expected Output Format")
            parts.append(
                "Your output should conform to this schema:\n"
                f"```json\n{json.dumps(self._step.output_schema, indent=2)}\n```"
            )

        if self._step.config:
            parts.append("\n## Step Configuration")
            parts.append(f"```json\n{json.dumps(self._step.config, indent=2)}\n```")

        return "\n".join(parts)

    def _build_posthook_context(self, work_response: str) -> str:
        """Build context for the posthook agent."""
        parts = [
            f"# Format Output for Pipeline Step: {self._step.name}",
            "\n## Work Step Response",
            f"The work agent produced the following response:\n\n{work_response}",
        ]
        if self._step.output_schema:
            parts.append(
                "\n## Required Output Schema\n"
                "Extract and format the response into valid JSON matching this schema:\n"
                f"```json\n{json.dumps(self._step.output_schema, indent=2)}\n```"
            )
        else:
            parts.append(
                "\n## Instructions\n"
                "Extract the structured output from the work response as valid JSON."
            )
        parts.append("\nRespond with ONLY the JSON output, no markdown fences or explanation.")
        return "\n".join(parts)

    async def _run_agent(
        self,
        agent_name: str,
        prompt: str,
        toolkit_metadata: dict[str, Any],
    ) -> str:
        """Spawn and run an agent, collecting the text response."""
        agent_def = get_agent_definition(agent_name)
        if agent_def is None:
            raise ValueError(f"Agent '{agent_name}' not found")

        # Inject pipeline_context toolkit if metadata provided
        if toolkit_metadata and "pipeline_context" not in (agent_def.toolkits or []):
            agent_def = agent_def.model_copy(
                update={"toolkits": [*(agent_def.toolkits or []), "pipeline_context"]}
            )

        # Reuse parent session — pipeline run has its own run_id for tracking
        agent = spawn_agent(
            self._session_config,
            messages=[],
            agent_def=agent_def,
        )

        # Collect text output
        text_parts: list[str] = []
        async for event in agent.run(prompt):
            if hasattr(event, "text") and event.text:
                text_parts.append(event.text)

        return "".join(text_parts)

    def _parse_output(self, text: str) -> dict[str, Any]:
        """Parse agent response as JSON.  Falls back to wrapping in a dict."""
        cleaned = text.strip()
        # Strip markdown code fences if present
        if cleaned.startswith("```"):
            lines = cleaned.split("\n")
            # Remove first and last fence lines
            if lines[0].startswith("```"):
                lines = lines[1:]
            if lines and lines[-1].strip() == "```":
                lines = lines[:-1]
            cleaned = "\n".join(lines).strip()

        try:
            parsed = json.loads(cleaned)
            if isinstance(parsed, dict):
                return parsed
            return {"result": parsed}
        except json.JSONDecodeError:
            return {"raw_output": text}
