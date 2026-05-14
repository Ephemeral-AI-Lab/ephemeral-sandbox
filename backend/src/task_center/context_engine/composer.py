"""ContextComposer — single launch entry point for every agent spawn.

The composer threads ``base_agent_name`` + :class:`ContextScope` through the
resolver, engine, and renderer in a fixed order:

    resolver.resolve → engine.build → packet.blocks.extend(...) →
    context_packet_store.insert → renderer.render → :class:`LaunchBundle`

That is the entire surface. Adding a new role means registering a recipe
and (optionally) declaring variants on its ``agent.md`` — no engine code
changes.
"""

from __future__ import annotations

from dataclasses import dataclass

from agents import AgentDefinition
from task_center.context_engine.engine import ContextEngine
from task_center.context_engine.packet import ContextPacket
from task_center.context_engine.renderer import MarkdownPromptRenderer
from task_center.agent_routing.resolver import RuleBasedAgentResolver
from task_center.context_engine.scope import ContextScope


@dataclass(frozen=True, slots=True)
class LaunchBundle:
    """The composer's output: everything the launcher needs."""

    agent_def: AgentDefinition
    rendered_prompt: str
    packet: ContextPacket
    context_packet_id: str | None


@dataclass(frozen=True, slots=True)
class ContextComposer:
    """Single launch entry point. Frozen so dependencies are explicit."""

    resolver: RuleBasedAgentResolver
    engine: ContextEngine
    renderer: MarkdownPromptRenderer

    @classmethod
    def default(
        cls,
        engine: ContextEngine,
    ) -> ContextComposer:
        return cls(
            resolver=RuleBasedAgentResolver(),
            engine=engine,
            renderer=MarkdownPromptRenderer(),
        )

    def compose(
        self, *, base_agent_name: str, scope: ContextScope
    ) -> LaunchBundle:
        selection = self.resolver.resolve(
            base_agent_name=base_agent_name,
            scope=scope,
            deps=self.engine.deps,
        )
        # ``resolver.resolve`` enforces context_recipe presence and raises
        # ``MissingContextRecipeError`` for both base and variant-target paths.
        packet = self.engine.build(selection.context_recipe, scope)
        if selection.required_context_blocks:
            packet.blocks.extend(selection.required_context_blocks)
        context_packet_id = self._persist(packet)
        rendered_prompt = self.renderer.render(packet)
        return LaunchBundle(
            agent_def=selection.agent_def,
            rendered_prompt=rendered_prompt,
            packet=packet,
            context_packet_id=context_packet_id,
        )

    # ---- internals ------------------------------------------------------

    def _persist(self, packet: ContextPacket) -> str | None:
        store = self.engine.deps.context_packet_store
        if store is None:
            return None
        return store.insert(packet)
