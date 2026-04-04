"""Model clients — Anthropic and OpenAI-compatible."""

from ephemeralos.models.clients.anthropic import AnthropicApiClient
from ephemeralos.models.clients.openai_compat import OpenAICompatibleClient

__all__ = ["AnthropicApiClient", "OpenAICompatibleClient"]
