"""ScopeChangeBuffer — per-executor notification buffer with replacement semantics.

Buffers scope change notifications from ScopeChangeListener and flushes
them as a single SystemReminderBlock at the top of each query loop turn.
Only one active notification exists in display_messages at a time —
previous notifications are marked superseded so the compactor can drop them.

See Section 14.7 of the coordination redesign doc.
"""

from __future__ import annotations

import logging
from typing import Any

from message.messages import ConversationMessage, SystemReminderBlock

logger = logging.getLogger(__name__)

SCOPE_CHANGE_CATEGORY = "scope_change"
SCOPE_CHANGE_SUPERSEDED = "scope_change_superseded"


class ScopeChangeBuffer:
    """Buffers scope change notifications for controlled injection.

    - ``buffer()`` is called by ``ScopeChangeListener._on_notify`` when a
      file in this executor's scope is edited by another agent.
    - ``flush_into()`` is called at the top of each query loop turn
      (alongside background task collection). It produces at most one
      ``SystemReminderBlock`` per flush and replaces any previous
      notification to prevent context accumulation.

    Anti-aggression safeguards:
    - **Minimum interval**: skips flush if fewer than ``min_turns_between``
      turns have passed since the last notification (changes stay buffered).
    - **Batched buffer**: accumulates changes across skipped turns and
      delivers them all in one coalesced notification when the interval
      elapses. Single-file edits that arrive in isolation are held until
      the next batch window rather than firing immediately.
    """

    def __init__(self, *, min_turns_between: int = 1) -> None:
        self._pending: dict[str, dict[str, str]] = {}  # file_path → latest change
        self._last_notification_idx: int | None = None
        self._min_turns_between = min_turns_between
        self._turns_since_last_flush = 0

    def buffer(self, change: dict[str, str]) -> None:
        """Buffer a scope change notification. Deduplicates by file_path.

        Called from the ScopeChangeListener's callback. Safe under the GIL —
        dict.__setitem__ is atomic with respect to the flush_into call on
        the same event loop. Changes accumulate across turns until the
        minimum interval elapses.
        """
        self._pending[change["file_path"]] = change

    def flush_into(self, display_messages: list[Any]) -> str | None:
        """Flush buffered changes into display_messages as one SystemReminderBlock.

        Called at the top of each query loop turn. Returns the injected
        notification text when a reminder was added, otherwise ``None``.

        Skips the flush if the minimum turn interval hasn't elapsed —
        changes stay in the buffer and are coalesced into the next
        notification. This prevents spamming the agent on every turn
        during bursts of concurrent edits.

        Replaces the previous scope_change notification (if any) by marking
        it as superseded — the compactor can safely drop superseded messages.
        """
        if not self._pending:
            # No changes — still count the turn for interval tracking.
            self._turns_since_last_flush += 1
            return None

        self._turns_since_last_flush += 1

        # Hold changes until minimum interval elapses (batched buffer).
        if self._turns_since_last_flush < self._min_turns_between:
            return None

        # Interval met — drain the entire buffer into one notification.
        changes = list(self._pending.values())
        self._pending.clear()
        self._turns_since_last_flush = 0

        lines = [
            f"- {c['file_path']} ({c.get('edit_type', 'edit')} by {c.get('agent_id', 'unknown')})"
            for c in changes
        ]
        text = (
            "Files in your scope were edited by other agents. "
            "Re-read before editing:\n" + "\n".join(lines)
        )

        # Mark previous notification as superseded so compactor can drop it.
        if self._last_notification_idx is not None:
            try:
                old_msg = display_messages[self._last_notification_idx]
                if (
                    old_msg.content
                    and hasattr(old_msg.content[0], "category")
                    and old_msg.content[0].category == SCOPE_CHANGE_CATEGORY
                ):
                    old_msg.content[0].category = SCOPE_CHANGE_SUPERSEDED
            except (IndexError, AttributeError):
                pass  # display_messages may have been compacted

        self._last_notification_idx = len(display_messages)
        display_messages.append(
            ConversationMessage(
                role="user",
                content=[SystemReminderBlock(category=SCOPE_CHANGE_CATEGORY, text=text)],
            )
        )
        logger.info(
            "[scope_buffer] flushed %d file change(s) into agent context",
            len(changes),
        )
        return text

    @property
    def has_pending(self) -> bool:
        return bool(self._pending)
