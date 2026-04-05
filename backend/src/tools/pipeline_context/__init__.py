"""Pipeline context toolkit — query the incremental context map during pipeline execution."""

from tools.base import BaseToolkit
from tools.pipeline_context.context_tools import (
    make_get_pipeline_metadata_tool,
    make_list_pipeline_steps_tool,
    make_query_pipeline_context_tool,
)


class PipelineContextToolkit(BaseToolkit):
    """Tools for querying pipeline context map and metadata."""

    def __init__(
        self,
        *,
        context_map: dict[str, dict] | None = None,
        pipeline_meta: dict | None = None,
        current_step: str | None = None,
    ) -> None:
        super().__init__(
            name="pipeline_context",
            description="Query pipeline context map and metadata",
            tools=[
                make_query_pipeline_context_tool(context_map=context_map),
                make_list_pipeline_steps_tool(context_map=context_map),
                make_get_pipeline_metadata_tool(
                    pipeline_meta=pipeline_meta,
                    current_step=current_step,
                ),
            ],
            instructions=(
                "Query the shared context map during sequential pipeline execution. "
                "Each step in the pipeline writes context that later steps can read.\n\n"
                "- `query_pipeline_context` — read context written by a previous step. "
                "Use to access outputs, decisions, or artifacts from earlier in the pipeline.\n"
                "- `list_pipeline_steps` — list all steps and their completion status. "
                "Use to understand where you are in the pipeline.\n"
                "- `get_pipeline_metadata` — read pipeline-level metadata (name, current step, config). "
                "Use to understand the overall pipeline goal and your role in it."
            ),
        )


__all__ = ["PipelineContextToolkit"]
