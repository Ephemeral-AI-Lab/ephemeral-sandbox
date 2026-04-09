"""Posthook execution: run a serializer agent after a work-phase agent.

A posthook is just *another registered agent*. The work-phase agent
declares ``posthook=PosthookConfig(agent_name="submit_plan_agent", ...)``
and the engine looks that name up in the agent registry, then runs it as
a normal ephemeral agent. The serializer agent's own AgentDefinition
controls its tool surface, model, max_turns, and prompt — there is no
parent-clone hack and no special-case fields on AgentDefinition.

Contract for posthook submit serializers:

* Pure submit serializers MUST NOT carry builtin skills (``skills == []``
  and ``include_skills is False``). A pure serializer is meant to do
  exactly one thing — call its submit tool — and a wider tool surface
  defeats the point. This is enforced at runtime by
  ``execute_with_posthook``.
* Decision-style posthooks that choose between multiple posthook tools
  (for example summary vs retry vs replan) may carry bundled skills. The
  no-skills contract applies only to pure submit serializers.
* They communicate the accepted submission back to the helper through a
  single string-keyed slot in ``ctx.tool_metadata`` (see
  ``PosthookConfig.metadata_key``). The submit tool reads
  ``tool_metadata['posthook_metadata_key']`` to know which slot to write.

The helper stays generic: ``team/`` is not imported here.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Awaitable, Callable

if TYPE_CHECKING:
    from agents.types import AgentDefinition

logger = logging.getLogger(__name__)


@dataclass
class PosthookConfig:
    """Pointer to a registered serializer agent.

    ``agent_name`` is looked up via the caller-supplied ``agent_lookup``
    callable (typically ``agents.registry.get_definition``). ``metadata_key``
    is the slot in ``ctx.tool_metadata`` that the submit tool writes to and
    that the helper reads back.
    """

    agent_name: str
    metadata_key: str = "submitted_output"


class PosthookError(Exception):
    """Base class for posthook lifecycle errors."""


class PosthookMisconfigured(PosthookError):
    """Configuration is invalid: missing dependencies, unregistered agent,
    or a pure submit serializer that violates the no-skills contract."""


class NoPosthookOutput(PosthookError):
    """Raised when the posthook agent never wrote an accepted submission."""


QueryRunner = Callable[["AgentDefinition", Any], Awaitable[Any]]
AgentLookup = Callable[[str], "AgentDefinition | None"]
PosthookCtxBuilder = Callable[["AgentDefinition", Any], Any]


def _stamp_metadata_key(ctx: Any, key: str) -> None:
    """Tell the submit tool which metadata slot to write its accepted payload into.

    The ctx contract requires ``ctx.tool_metadata`` to be a mutable dict;
    callers that pass anything else are buggy and we want that to surface
    loudly rather than be silently swallowed.
    """
    setter = getattr(ctx, "set_posthook_metadata_key", None)
    if callable(setter):
        setter(key)
        return
    ctx.tool_metadata["posthook_metadata_key"] = key


def _read_submitted_output(ctx: Any, key: str) -> Any | None:
    reader = getattr(ctx, "get_posthook_output", None)
    if callable(reader):
        return reader(key)
    return ctx.tool_metadata.get(key)


def _is_pure_submit_posthook(defn: "AgentDefinition") -> bool:
    """Return whether *defn* is a pure submit serializer posthook.

    Builtin pure serializers use exactly one ``submit_*`` toolkit, while
    decision posthooks use ``posthook_*`` multi-tool toolkits. Some legacy
    decision agents still use decision-prefixed names with serializer-shaped
    toolkits; those must not be treated as pure submitters for the no-skills
    guard.
    """
    name = str(getattr(defn, "name", "") or "")
    if name.startswith("decision_"):
        return False
    toolkits = [str(tk) for tk in (getattr(defn, "toolkits", None) or [])]
    return bool(toolkits) and all(tk.startswith("submit_") for tk in toolkits)


def _assert_serializer_has_no_skills(defn: "AgentDefinition") -> None:
    """Pure submit serializers must not carry builtin skills.

    A serializer agent exists to call exactly one submit tool. Builtin
    skills broaden its tool surface and invite the model to wander —
    which is the entire failure mode the dedicated submit phase was
    designed to prevent. Reject at lookup time so misconfigurations
    fail before the work phase burns budget on the next call.
    """
    if not _is_pure_submit_posthook(defn):
        return
    if getattr(defn, "include_skills", False) or getattr(defn, "skills", None):
        raise PosthookMisconfigured(
            f"pure submit posthook agent {defn.name!r} must not be equipped "
            f"with builtin skills (include_skills must be False and skills "
            f"must be empty); "
            f"got include_skills={defn.include_skills!r}, skills={defn.skills!r}"
        )


async def execute_with_posthook(
    work_defn: "AgentDefinition",
    work_ctx: Any,
    *,
    runner: QueryRunner,
    agent_lookup: AgentLookup | None = None,
    posthook_ctx_builder: PosthookCtxBuilder | None = None,
) -> tuple[Any, Any | None]:
    """Run the work phase; if posthook is configured, run the serializer agent.

    Returns ``(work_result, submitted_output | None)``.

    Raises:
        PosthookMisconfigured: posthook is configured but ``agent_lookup``
            or ``posthook_ctx_builder`` was not supplied, the named
            posthook agent is not registered, or a pure submit serializer
            carries builtin skills. Raised *before* the work phase runs
            whenever possible so misconfigurations don't burn the work budget.
        NoPosthookOutput: serializer ran but never wrote an accepted
            submission to ``ctx.tool_metadata[metadata_key]``.
    """
    cfg: PosthookConfig | None = work_defn.posthook

    # Eager validation: if a posthook is configured, fail before running
    # the work phase rather than after. The work phase can be expensive.
    if cfg is not None:
        if agent_lookup is None or posthook_ctx_builder is None:
            raise PosthookMisconfigured(
                f"work agent {work_defn.name!r} declares posthook "
                f"{cfg.agent_name!r} but agent_lookup or posthook_ctx_builder "
                f"was not supplied to execute_with_posthook"
            )
        posthook_defn = agent_lookup(cfg.agent_name)
        if posthook_defn is None:
            raise PosthookMisconfigured(
                f"posthook agent {cfg.agent_name!r} (declared by "
                f"{work_defn.name!r}) is not registered"
            )
        _assert_serializer_has_no_skills(posthook_defn)
        _stamp_metadata_key(work_ctx, cfg.metadata_key)
    else:
        posthook_defn = None

    work_result = await runner(work_defn, work_ctx)

    if cfg is None:
        return work_result, None

    # If the work phase already submitted (e.g. its toolkit included the
    # submit tool directly), skip the posthook entirely. Logged because
    # this branch silently changes the agent lifecycle and is otherwise
    # invisible.
    if _read_submitted_output(work_ctx, cfg.metadata_key) is not None:
        logger.debug(
            "execute_with_posthook: work agent %r already submitted to %r; "
            "skipping posthook %r",
            work_defn.name,
            cfg.metadata_key,
            cfg.agent_name,
        )
        return work_result, _read_submitted_output(work_ctx, cfg.metadata_key)

    assert posthook_defn is not None  # established above when cfg is not None
    posthook_ctx = posthook_ctx_builder(posthook_defn, work_result)  # type: ignore[misc]
    _stamp_metadata_key(posthook_ctx, cfg.metadata_key)

    await runner(posthook_defn, posthook_ctx)

    submitted = _read_submitted_output(posthook_ctx, cfg.metadata_key)
    if submitted is None:
        raise NoPosthookOutput(
            f"Posthook agent {cfg.agent_name!r} for {work_defn.name!r} ended "
            f"without writing {cfg.metadata_key!r}."
        )
    return work_result, submitted
