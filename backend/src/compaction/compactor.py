"""Conversation compaction — microcompact and full LLM-based summarization.

Faithfully translated from Claude Code's compaction system:
- Microcompact: clear old tool result content to reduce token count cheaply
- Full compact: call the LLM to produce a structured summary of older messages
- Auto-compact: trigger compaction automatically when token count exceeds threshold
"""

from __future__ import annotations

import copy
import logging
import math
import re
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from providers.types import SupportsStreamingMessages

from compaction.prompts import (
    NO_TOOLS_PREAMBLE,
    BASE_COMPACT_PROMPT,
    NO_TOOLS_TRAILER,
)
from message import (
    BackgroundTaskStateBlock,
    ConversationMessage,
    ContentBlock,
    ThinkingBlock,
    SystemReminderBlock,
    TextBlock,
    ToolResultBlock,
    ToolUseBlock,
    serialize_content_block,
)
from tools.builtins.background._common import (
    build_background_snapshot_metadata,
    render_background_snapshot,
)

log = logging.getLogger(__name__)


def estimate_tokens(text: str) -> int:
    """Estimate tokens from plain text using a rough character heuristic."""
    if not text:
        return 0
    return max(1, (len(text) + 3) // 4)


# ---------------------------------------------------------------------------
# Constants (from Claude Code microCompact.ts / autoCompact.ts)
# ---------------------------------------------------------------------------

COMPACTABLE_TOOLS: frozenset[str] = frozenset(
    {
        "read_file",
        "bash",
        "grep",
        "glob",
        "web_search",
        "web_fetch",
        "edit_file",
        "write_file",
    }
)

TIME_BASED_MC_CLEARED_MESSAGE = "[Old tool result content cleared]"

# Auto-compact thresholds
AUTOCOMPACT_BUFFER_TOKENS = 13_000
MAX_OUTPUT_TOKENS_FOR_SUMMARY = 20_000
MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES = 3

# Microcompact defaults
DEFAULT_KEEP_RECENT = 5


# Token estimation padding (conservative)
TOKEN_ESTIMATION_PADDING = 4 / 3

# Default context windows per model family
_DEFAULT_CONTEXT_WINDOW = 200_000
_BACKGROUND_SNAPSHOT_TOOLS: frozenset[str] = frozenset(
    {"check_background_progress", "wait_for_background_task"}
)
_REDUCIBLE_RUNNING_STATUSES: frozenset[str] = frozenset({"running"})
_REDUCIBLE_TERMINAL_STATUSES: frozenset[str] = frozenset(
    {"completed", "failed", "cancelled", "delivered"}
)
_REDUCIBLE_STATUSES: frozenset[str] = _REDUCIBLE_RUNNING_STATUSES | _REDUCIBLE_TERMINAL_STATUSES


# ---------------------------------------------------------------------------
# Token estimation
# ---------------------------------------------------------------------------


def estimate_message_tokens(messages: list[ConversationMessage]) -> int:
    """Estimate total tokens for a conversation, including the 4/3 padding."""
    total = 0
    for msg in messages:
        for block in msg.content:
            if isinstance(block, ThinkingBlock):
                continue
            if isinstance(block, TextBlock):
                total += estimate_tokens(block.text)
            elif isinstance(block, (SystemReminderBlock, BackgroundTaskStateBlock)):
                total += estimate_tokens(serialize_content_block(block)["text"])
            elif isinstance(block, ToolResultBlock):
                total += estimate_tokens(block.content)
            elif isinstance(block, ToolUseBlock):
                total += estimate_tokens(block.name)
                total += estimate_tokens(str(block.input))
    return math.ceil(total * TOKEN_ESTIMATION_PADDING)


def _background_snapshot_info(
    block: ToolResultBlock,
    tool_use_map: dict[str, tuple[int, int, str]],
) -> dict[str, Any] | None:
    if not block.metadata:
        return None
    snapshot = block.metadata.get("background_snapshot")
    if not isinstance(snapshot, dict):
        return None
    tool_use = tool_use_map.get(block.tool_use_id)
    if tool_use is None or tool_use[2] not in _BACKGROUND_SNAPSHOT_TOOLS:
        return None
    statuses = snapshot.get("statuses")
    kind = snapshot.get("kind")
    scope = snapshot.get("scope")
    if not isinstance(statuses, list) or not isinstance(kind, str) or not isinstance(scope, str):
        return None
    elapsed = snapshot.get("elapsed_seconds")
    if not isinstance(elapsed, (int, float)):
        elapsed = None
    return {
        "kind": kind,
        "scope": scope,
        "statuses": statuses,
        "elapsed_seconds": elapsed,
    }


def reduce_for_api(display_messages: list[ConversationMessage]) -> list[ConversationMessage]:
    """Return a reduced provider view that keeps only the latest task state.

    For each background task_id, keeps only the most recent (or terminal)
    state block / snapshot entry. All older duplicates are dropped.
    """
    # Pass 1: build tool_use map and find the "winner" location for each task.
    # Winner = latest occurrence, preferring terminal statuses.
    tool_use_map: dict[str, tuple[int, int, str]] = {}
    snapshot_tool_use_ids: set[str] = set()
    _WinnerKey = tuple[bool, int, int, int, tuple[int, int] | None, str | None, int | None]
    winners: dict[str, _WinnerKey] = {}

    for msg_idx, msg in enumerate(display_messages):
        if msg.role == "assistant":
            for block_idx, block in enumerate(msg.content):
                if isinstance(block, ToolUseBlock):
                    tool_use_map[block.id] = (msg_idx, block_idx, block.name)

    key: _WinnerKey
    for msg_idx, msg in enumerate(display_messages):
        for block_idx, block in enumerate(msg.content):
            if isinstance(block, BackgroundTaskStateBlock) and block.status in _REDUCIBLE_STATUSES:
                is_term = block.status in _REDUCIBLE_TERMINAL_STATUSES
                key = (is_term, msg_idx, block_idx, -1, (msg_idx, block_idx), None, None)
                cur = winners.get(block.task_id)
                if cur is None or key[:4] > cur[:4]:
                    winners[block.task_id] = key
                continue
            if not isinstance(block, ToolResultBlock):
                continue
            snapshot = _background_snapshot_info(block, tool_use_map)
            if snapshot is None:
                continue
            snapshot_tool_use_ids.add(block.tool_use_id)
            for si, entry in enumerate(snapshot["statuses"]):
                tid = entry.get("task_id")
                status = entry.get("status")
                if not isinstance(tid, str) or status not in _REDUCIBLE_STATUSES:
                    continue
                is_term = status in _REDUCIBLE_TERMINAL_STATUSES
                key = (is_term, msg_idx, block_idx, si, None, block.tool_use_id, si)
                cur = winners.get(tid)
                if cur is None or key[:4] > cur[:4]:
                    winners[tid] = key

    # Build keep-sets from winners.
    keep_state_blocks: set[tuple[int, int]] = set()
    keep_snapshot_statuses: dict[str, set[int]] = {}
    for w in winners.values():
        if w[4] is not None:  # block_ref
            keep_state_blocks.add(w[4])
        if w[5] is not None and w[6] is not None:  # tool_use_id, status_idx
            keep_snapshot_statuses.setdefault(w[5], set()).add(w[6])

    drop_tool_use_ids = snapshot_tool_use_ids - keep_snapshot_statuses.keys()

    # Pass 2: rebuild messages keeping only winners.
    reduced: list[ConversationMessage] = []
    for msg_idx, msg in enumerate(display_messages):
        new_content: list[ContentBlock] = []
        for block_idx, block in enumerate(msg.content):
            if isinstance(block, BackgroundTaskStateBlock):
                if (msg_idx, block_idx) in keep_state_blocks:
                    new_content.append(block.model_copy(deep=True))
                continue
            if isinstance(block, ToolUseBlock) and block.id in drop_tool_use_ids:
                continue
            if isinstance(block, ToolResultBlock):
                snapshot = _background_snapshot_info(block, tool_use_map)
                if snapshot is None:
                    new_content.append(block.model_copy(deep=True))
                    continue
                keep_idxs = keep_snapshot_statuses.get(block.tool_use_id)
                if not keep_idxs:
                    continue
                filtered = [
                    copy.deepcopy(s) for i, s in enumerate(snapshot["statuses"])
                    if i in keep_idxs
                ]
                rebuilt = block.model_copy(deep=True)
                rebuilt.content = render_background_snapshot(
                    snapshot["kind"], filtered, elapsed_seconds=snapshot["elapsed_seconds"],
                )
                rebuilt.metadata = build_background_snapshot_metadata(
                    snapshot["kind"], snapshot["scope"], filtered,
                    elapsed_seconds=snapshot["elapsed_seconds"],
                )
                new_content.append(rebuilt)
                continue
            new_content.append(block.model_copy(deep=True))
        if new_content:
            reduced.append(ConversationMessage(role=msg.role, content=new_content))
    return reduced


def _message_tool_use_ids(message: ConversationMessage) -> set[str]:
    return {block.id for block in message.content if isinstance(block, ToolUseBlock)}


def _message_tool_result_ids(message: ConversationMessage) -> set[str]:
    return {block.tool_use_id for block in message.content if isinstance(block, ToolResultBlock)}


def _preserve_recent_split_index(
    messages: list[ConversationMessage],
    preserve_recent: int,
) -> int:
    """Return a split index that keeps provider-visible tool sequencing valid."""

    def _tool_sequence_valid(msgs: list[ConversationMessage]) -> bool:
        pending: set[str] = set()
        for msg in msgs:
            tool_results = _message_tool_result_ids(msg)
            if pending:
                if msg.role != "user" or not pending.issubset(tool_results):
                    return False
                if tool_results - pending:
                    return False
                pending.clear()
            elif tool_results:
                return False
            pending = _message_tool_use_ids(msg)
        return not pending

    split_idx = max(0, len(messages) - max(0, preserve_recent))
    while 0 < split_idx < len(messages):
        if _tool_sequence_valid(messages[:split_idx]) and _tool_sequence_valid(
            messages[split_idx:]
        ):
            break
        split_idx -= 1
    return split_idx


def _walk_tool_sequence(
    messages: list[ConversationMessage],
    *,
    strict: bool = False,
) -> str | None:
    """Validate or sanitize tool-use/result pairing in a single pass.

    When *strict* is ``True``, return a human-readable error on the first
    violation (used before sending to the summary LLM). When ``False``,
    silently strip orphaned tool_use / tool_result blocks in-place so the
    provider never sees invalid sequencing.

    Returns an error string in strict mode, or ``None`` when valid / sanitized.
    """
    pending_ids: set[str] = set()
    pending_msg_idx: int | None = None

    def _strip_tool_uses(idx: int | None, ids: set[str]) -> None:
        if idx is None or not ids:
            return
        msg = messages[idx]
        msg.content = [
            b for b in msg.content
            if not (isinstance(b, ToolUseBlock) and b.id in ids)
        ]

    for msg_idx, message in enumerate(messages):
        tool_use_ids = _message_tool_use_ids(message)
        tool_result_ids = _message_tool_result_ids(message)
        satisfied_pending = False

        if pending_ids:
            if strict:
                if message.role != "user":
                    return (
                        f"assistant tool_use at index {pending_msg_idx} "
                        f"followed by non-user at index {msg_idx}"
                    )
                missing = pending_ids - tool_result_ids
                if missing:
                    return (
                        f"assistant tool_use at index {pending_msg_idx} "
                        f"missing tool_results {sorted(missing)} at index {msg_idx}"
                    )
                extra = tool_result_ids - pending_ids
                if extra:
                    return (
                        f"unexpected tool_results {sorted(extra)} "
                        f"at index {msg_idx}"
                    )
                pending_ids = set()
                pending_msg_idx = None
                satisfied_pending = True
            else:
                if message.role != "user" or not pending_ids.issubset(tool_result_ids):
                    _strip_tool_uses(pending_msg_idx, pending_ids)
                    pending_ids = set()
                    pending_msg_idx = None
                    tool_result_ids = _message_tool_result_ids(message)
                else:
                    extra = tool_result_ids - pending_ids
                    if extra:
                        message.content = [
                            b for b in message.content
                            if not (isinstance(b, ToolResultBlock) and b.tool_use_id in extra)
                        ]
                    pending_ids = set()
                    pending_msg_idx = None
                    tool_result_ids = _message_tool_result_ids(message)
                    satisfied_pending = True

        if tool_result_ids and not tool_use_ids and not satisfied_pending:
            if strict:
                return (
                    f"orphaned tool_results {sorted(tool_result_ids)} at index {msg_idx}"
                )
            message.content = [
                b for b in message.content if not isinstance(b, ToolResultBlock)
            ]

        tool_use_ids = _message_tool_use_ids(message)
        if tool_use_ids:
            pending_ids = set(tool_use_ids)
            pending_msg_idx = msg_idx

    if pending_ids:
        if strict:
            return (
                f"assistant tool_use at index {pending_msg_idx} "
                f"missing trailing tool_results for {sorted(pending_ids)}"
            )
        _strip_tool_uses(pending_msg_idx, pending_ids)

    return None


def _sanitize_tool_sequence(messages: list[ConversationMessage]) -> list[ConversationMessage]:
    """Drop malformed stale tool-use/result blocks from the provider view."""
    sanitized = copy.deepcopy(messages)
    _walk_tool_sequence(sanitized, strict=False)
    return [msg for msg in sanitized if msg.content]


# ---------------------------------------------------------------------------
# Microcompact — clear old tool results to reduce tokens cheaply
# ---------------------------------------------------------------------------


def _collect_compactable_tool_ids(messages: list[ConversationMessage]) -> list[str]:
    """Walk messages and collect tool_use IDs whose results are compactable."""
    ids: list[str] = []
    for msg in messages:
        if msg.role != "assistant":
            continue
        for block in msg.content:
            if isinstance(block, ToolUseBlock) and block.name in COMPACTABLE_TOOLS:
                ids.append(block.id)
    return ids


def microcompact_messages(
    messages: list[ConversationMessage],
    *,
    keep_recent: int = DEFAULT_KEEP_RECENT,
) -> tuple[list[ConversationMessage], int]:
    """Clear old compactable tool results, keeping the most recent *keep_recent*.

    This is the cheap first pass — no LLM call required. Tool result content
    is replaced with :data:`TIME_BASED_MC_CLEARED_MESSAGE`.

    Each message's ``content`` list is rebuilt (the cleared
    ``ToolResultBlock`` is a fresh instance), but the messages themselves are
    mutated in place — i.e. the same ``messages`` list is returned with the
    same ``ConversationMessage`` objects.

    Returns:
        (messages, tokens_saved)
    """
    keep_recent = max(1, keep_recent)  # never clear ALL results
    all_ids = _collect_compactable_tool_ids(messages)

    if len(all_ids) <= keep_recent:
        return messages, 0

    keep_set = set(all_ids[-keep_recent:])
    clear_set = set(all_ids) - keep_set

    tokens_saved = 0
    for msg in messages:
        if msg.role != "user":
            continue
        new_content: list[ContentBlock] = []
        for block in msg.content:
            if (
                isinstance(block, ToolResultBlock)
                and block.tool_use_id in clear_set
                and block.content != TIME_BASED_MC_CLEARED_MESSAGE
            ):
                tokens_saved += estimate_tokens(block.content)
                new_content.append(
                    ToolResultBlock(
                        tool_use_id=block.tool_use_id,
                        content=TIME_BASED_MC_CLEARED_MESSAGE,
                        is_error=block.is_error,
                        metadata=copy.deepcopy(block.metadata),
                    )
                )
            else:
                new_content.append(block)
        msg.content = new_content

    if tokens_saved > 0:
        log.info(
            "Microcompact cleared %d tool results, saved ~%d tokens", len(clear_set), tokens_saved
        )

    return messages, tokens_saved


def get_compact_prompt(custom_instructions: str | None = None) -> str:
    """Build the full compaction prompt sent to the model."""
    prompt = NO_TOOLS_PREAMBLE + BASE_COMPACT_PROMPT
    if custom_instructions and custom_instructions.strip():
        prompt += f"\n\nAdditional Instructions:\n{custom_instructions}"
    prompt += NO_TOOLS_TRAILER
    return prompt


def format_compact_summary(raw_summary: str) -> str:
    """Strip the <analysis> scratchpad and extract the <summary> content."""
    text = re.sub(r"<analysis>[\s\S]*?</analysis>", "", raw_summary)
    m = re.search(r"<summary>([\s\S]*?)</summary>", text)
    if m:
        text = text.replace(m.group(0), f"Summary:\n{m.group(1).strip()}")
    text = re.sub(r"\n\n+", "\n\n", text)
    return text.strip()


def build_compact_summary_message(
    summary: str,
    *,
    suppress_follow_up: bool = False,
    recent_preserved: bool = False,
) -> str:
    """Create the injected user message that replaces compacted history."""
    formatted = format_compact_summary(summary)
    text = (
        "This session is being continued from a previous conversation that ran "
        "out of context. The summary below covers the earlier portion of the "
        "conversation.\n\n"
        f"{formatted}"
    )
    if recent_preserved:
        text += "\n\nRecent messages are preserved verbatim."
    if suppress_follow_up:
        text += (
            "\nContinue the conversation from where it left off without asking "
            "the user any further questions. Resume directly — do not acknowledge "
            "the summary, do not recap what was happening, do not preface with "
            '"I\'ll continue" or similar. Pick up the last task as if the break '
            "never happened."
        )
    return text


# ---------------------------------------------------------------------------
# Auto-compact tracking
# ---------------------------------------------------------------------------


@dataclass
class SessionState:
    """Mutable state that persists across ephemeral agent runs.

    Stored in the DB as part of the session record so compaction
    decisions carry over between requests.
    """

    compacted: bool = False
    turn_counter: int = 0
    consecutive_failures: int = 0

    def to_dict(self) -> dict:
        return {
            "compacted": self.compacted,
            "turn_counter": self.turn_counter,
            "consecutive_failures": self.consecutive_failures,
        }

    @classmethod
    def from_dict(cls, data: dict | None) -> SessionState:
        if not data:
            return cls()
        return cls(
            compacted=data.get("compacted", False),
            turn_counter=data.get("turn_counter", 0),
            consecutive_failures=data.get("consecutive_failures", 0),
        )


# ---------------------------------------------------------------------------
# Context window helpers
# ---------------------------------------------------------------------------


def get_autocompact_threshold(model: str) -> int:  # noqa: ARG001 — model reserved for future per-family windows
    """Calculate the token count at which auto-compact fires.

    The ``model`` argument is currently unused — all supported Claude models
    share the same window — but is preserved so callers can opt into
    per-family sizing later without an API break.
    """
    effective = _DEFAULT_CONTEXT_WINDOW - MAX_OUTPUT_TOKENS_FOR_SUMMARY
    return effective - AUTOCOMPACT_BUFFER_TOKENS


def should_autocompact(
    messages: list[ConversationMessage],
    model: str,
    state: SessionState,
) -> bool:
    """Return True when the conversation should be auto-compacted."""
    if state.consecutive_failures >= MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES:
        return False
    token_count = estimate_message_tokens(messages)
    threshold = get_autocompact_threshold(model)
    return token_count >= threshold


# ---------------------------------------------------------------------------
# Full compact execution (calls the LLM)
# ---------------------------------------------------------------------------


async def compact_conversation(
    messages: list[ConversationMessage],
    *,
    api_client: SupportsStreamingMessages,
    model: str,
    system_prompt: str = "",
    preserve_recent: int = 6,
    custom_instructions: str | None = None,
    suppress_follow_up: bool = True,
    skip_microcompact: bool = False,
) -> list[ConversationMessage]:
    """Compact messages by calling the LLM to produce a summary.

    1. Microcompact first (cheap token reduction) unless ``skip_microcompact``
       is set — ``compact_for_api`` already runs microcompact and passes
       ``skip_microcompact=True`` to avoid the redundant pass.
    2. Split into older (to summarize) and recent (to preserve).
    3. Call the LLM with the compact prompt to get a structured summary.
    4. Replace older messages with the summary + preserved recent messages.

    Args:
        messages: The full conversation history.
        api_client: An API client implementing SupportsStreamingMessages for the summary call.
        model: Model ID to use for the summary.
        system_prompt: System prompt for the summary call.
        preserve_recent: Number of recent messages to keep verbatim.
        custom_instructions: Optional extra instructions for the summary prompt.
        suppress_follow_up: If True, instruct the model not to ask follow-ups.

    Returns:
        The new compacted message list.
    """
    from providers.types import ApiMessageRequest, ApiMessageCompleteEvent

    if len(messages) <= preserve_recent:
        return list(messages)

    # Step 1: microcompact to reduce tokens cheaply (skipped when the caller
    # already ran microcompact — see compact_for_api).
    if not skip_microcompact:
        microcompact_messages(messages, keep_recent=DEFAULT_KEEP_RECENT)

    sequence_error = _walk_tool_sequence(messages, strict=True)
    if sequence_error is not None:
        raise ValueError(
            f"compaction preflight rejected malformed tool sequencing: {sequence_error}"
        )

    pre_compact_tokens = estimate_message_tokens(messages)
    log.info("Compacting conversation: %d messages, ~%d tokens", len(messages), pre_compact_tokens)

    # Step 2: split into older (summarize) and newer (preserve)
    split_idx = _preserve_recent_split_index(messages, preserve_recent)
    if split_idx <= 0:
        return list(messages)
    older = messages[:split_idx]
    newer = messages[split_idx:]

    # Step 3: build compact request — send older messages + compact prompt
    compact_prompt = get_compact_prompt(custom_instructions)
    compact_messages = list(older) + [ConversationMessage.from_user_text(compact_prompt)]
    sequence_error = _walk_tool_sequence(compact_messages, strict=True)
    if sequence_error is not None:
        raise ValueError(
            f"compaction preflight rejected malformed tool sequencing: {sequence_error}"
        )

    summary_text = ""
    async for event in api_client.stream_message(
        ApiMessageRequest(
            model=model,
            messages=compact_messages,
            system_prompt=system_prompt or "You are a conversation summarizer.",
            max_tokens=MAX_OUTPUT_TOKENS_FOR_SUMMARY,
            tools=[],  # no tools for compact call
        )
    ):
        if isinstance(event, ApiMessageCompleteEvent):
            summary_text = event.message.text

    if not summary_text:
        log.warning("Compact summary was empty — returning original messages")
        return messages

    # Step 4: build the new message list
    summary_content = build_compact_summary_message(
        summary_text,
        suppress_follow_up=suppress_follow_up,
        recent_preserved=len(newer) > 0,
    )
    summary_msg = ConversationMessage.from_user_text(summary_content)

    result = [summary_msg, *newer]
    post_compact_tokens = estimate_message_tokens(result)
    log.info(
        "Compaction done: %d -> %d messages, ~%d -> ~%d tokens (saved ~%d)",
        len(messages),
        len(result),
        pre_compact_tokens,
        post_compact_tokens,
        pre_compact_tokens - post_compact_tokens,
    )
    return result


# ---------------------------------------------------------------------------
# Auto-compact integration (called from query loop)
# ---------------------------------------------------------------------------


async def compact_for_api(
    display_messages: list[ConversationMessage],
    *,
    api_client: SupportsStreamingMessages,
    model: str,
    system_prompt: str = "",
    state: SessionState,
    preserve_recent: int = 6,
) -> list[ConversationMessage]:
    """Build the compacted message list to send to the LLM provider.

    Pure function: never mutates *display_messages*. Always returns a fresh
    list. The returned list is the "api_messages" view — the only list that
    should be passed to ``api_client.stream_message``.

    Compaction strategy:

    1. If token count is below the auto-compact threshold, return a shallow
       copy of *display_messages* unchanged.
    2. Otherwise, deep-copy *display_messages* and run microcompact on the
       copy. If that brings token count below the threshold, return it.
    3. Otherwise, run a full LLM-based compaction on the copy and return the
       resulting summarized list.
    4. On compaction failure, return the microcompacted copy and increment
       ``state.consecutive_failures``. *display_messages* is never touched.

    Args:
        display_messages: The full, append-only conversation history. Never
            mutated.
        api_client: API client used for the optional summary call.
        model: Model id (drives the token threshold).
        system_prompt: System prompt for the summary call.
        state: Mutable session state — only ``compacted``, ``turn_counter``,
            and ``consecutive_failures`` are updated; messages are not.
        preserve_recent: Number of recent messages to keep verbatim during
            full compaction.

    Returns:
        A new ``list[ConversationMessage]`` ready to send to the provider.
    """
    reduced = _sanitize_tool_sequence(reduce_for_api(display_messages))
    if not should_autocompact(reduced, model, state):
        return reduced

    log.info(
        "compact_for_api: auto-compact triggered (failures=%d)",
        state.consecutive_failures,
    )

    working = copy.deepcopy(reduced)
    working, _ = microcompact_messages(working)
    if not should_autocompact(working, model, state):
        log.info(
            "compact_for_api: background reduction/microcompact avoided full compact",
        )
        return working

    try:
        result = await compact_conversation(
            working,
            api_client=api_client,
            model=model,
            system_prompt=system_prompt,
            preserve_recent=preserve_recent,
            suppress_follow_up=True,
            skip_microcompact=True,
        )
        state.compacted = True
        state.turn_counter += 1
        state.consecutive_failures = 0
        return result
    except Exception as exc:
        state.consecutive_failures += 1
        log.error(
            "compact_for_api: full compact failed (attempt %d/%d): %s",
            state.consecutive_failures,
            MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES,
            exc,
        )
        return working


__all__ = [
    "AUTOCOMPACT_BUFFER_TOKENS",
    "COMPACTABLE_TOOLS",
    "TIME_BASED_MC_CLEARED_MESSAGE",
    "SessionState",
    "build_compact_summary_message",
    "compact_conversation",
    "compact_for_api",
    "estimate_message_tokens",
    "format_compact_summary",
    "get_autocompact_threshold",
    "get_compact_prompt",
    "microcompact_messages",
    "reduce_for_api",
    "should_autocompact",
]
