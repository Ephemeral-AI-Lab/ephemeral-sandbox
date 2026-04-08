"""Conversation compaction — microcompact and full LLM-based summarization."""

from compaction.compactor import (
    AUTOCOMPACT_BUFFER_TOKENS,
    COMPACTABLE_TOOLS,
    MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES,
    MAX_OUTPUT_TOKENS_FOR_SUMMARY,
    TIME_BASED_MC_CLEARED_MESSAGE,
    SessionState,
    build_compact_summary_message,
    compact_for_api,
    estimate_message_tokens,
    format_compact_summary,
    get_autocompact_threshold,
    get_compact_prompt,
    microcompact_messages,
    reduce_for_api,
    should_autocompact,
)

__all__ = [
    "AUTOCOMPACT_BUFFER_TOKENS",
    "COMPACTABLE_TOOLS",
    "MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES",
    "MAX_OUTPUT_TOKENS_FOR_SUMMARY",
    "TIME_BASED_MC_CLEARED_MESSAGE",
    "SessionState",
    "build_compact_summary_message",
    "compact_for_api",
    "estimate_message_tokens",
    "format_compact_summary",
    "get_autocompact_threshold",
    "get_compact_prompt",
    "microcompact_messages",
    "reduce_for_api",
    "should_autocompact",
]
