"""Notification rule abstraction and generic dispatcher.

Rules are the single source of truth for engine-generated `<system-reminder>`
content. Each rule's `trigger` is evaluated at the top of every model turn
by `dispatch_rules`; when it returns True, `body` produces the reminder text
and the result is pushed into the run's notification pool via
`service.notify_system`.

Rules live in `backend/src/notification/library/` (factories) and are
assembled into per-agent lists on `AgentDefinition.notification_rules`.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Callable

from message.messages import ConversationMessage

if TYPE_CHECKING:
    from engine.api import QueryContext
    from notification._runtime import SystemNotificationService


MessageList = list[ConversationMessage]

# Note: ``body`` and ``trigger`` receive ``(messages, QueryContext)`` at
# runtime. The type is loosened to ``Callable[..., ...]`` so that Pydantic
# (which validates ``AgentDefinition.notification_rules: list[NotificationRule]``)
# does not try to resolve the forward reference to ``QueryContext`` and
# raise ``PydanticUserError: not fully defined``.
RuleBody = Callable[..., str]
RuleTrigger = Callable[..., bool]


@dataclass(frozen=True)
class NotificationRule:
    """Declarative rule for emitting a `<system-reminder>` block.

    `trigger` and `body` both receive `(messages, context)` so rules can
    inspect the live transcript, agent identity, tool budget, and per-rule
    scratchpad without a separate context wrapper.

    `fire_once=True` (the default) skips the rule once its `name` is in the
    run's `notification_fired` set. Rules that need to fire repeatedly
    (e.g., budget warnings at multiple thresholds) set `fire_once=False`
    and manage their own dedup via `context.notification_state[name]`.
    """

    name: str
    body: RuleBody
    trigger: RuleTrigger
    fire_once: bool = True


async def dispatch_rules(
    rules: list[NotificationRule],
    messages: MessageList,
    context: "QueryContext",
    service: "SystemNotificationService",
    fired: set[str],
) -> None:
    """Evaluate `rules` in list order; emit each rule whose trigger fires.

    Called once per model turn from `_run_query_loop` before the next
    provider request is built. Rules fire in list order. Earlier rules'
    emissions land in the notification pool but do not appear in
    `messages` until the caller drains the pool, so a later rule's
    `trigger` cannot observe an earlier rule's reminder this turn.
    """
    for rule in rules:
        if rule.fire_once and rule.name in fired:
            continue
        if not rule.trigger(messages, context):
            continue
        text = rule.body(messages, context)
        if not text.strip():
            continue
        await service.notify_system(text)
        fired.add(rule.name)
