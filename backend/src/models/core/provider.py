"""Provider/auth capability helpers and API client factory."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from config.settings import Settings
from models.core.types import SupportsStreamingMessages


@dataclass(frozen=True)
class ProviderInfo:
    """Resolved provider metadata for UI and diagnostics."""

    name: str
    auth_kind: str
    voice_supported: bool
    voice_reason: str


def detect_provider(settings: Settings) -> ProviderInfo:
    """Infer the active provider and rough capability set."""
    base_url = (settings.base_url or "").lower()
    model = settings.model.lower()
    if "moonshot" in base_url or model.startswith("kimi"):
        return ProviderInfo(
            name="moonshot-anthropic-compatible",
            auth_kind="api_key",
            voice_supported=False,
            voice_reason="voice mode requires a Claude.ai-style authenticated voice backend",
        )
    if "dashscope" in base_url or model.startswith("qwen"):
        return ProviderInfo(
            name="dashscope-openai-compatible",
            auth_kind="api_key",
            voice_supported=False,
            voice_reason="voice mode is not supported for DashScope providers",
        )
    if "models.inference.ai.azure.com" in base_url or "github" in base_url:
        return ProviderInfo(
            name="github-models-openai-compatible",
            auth_kind="api_key",
            voice_supported=False,
            voice_reason="voice mode is not supported for GitHub Models",
        )
    if "bedrock" in base_url:
        return ProviderInfo(
            name="bedrock-compatible",
            auth_kind="aws",
            voice_supported=False,
            voice_reason="voice mode is not wired for Bedrock in this build",
        )
    if "vertex" in base_url or "aiplatform" in base_url:
        return ProviderInfo(
            name="vertex-compatible",
            auth_kind="gcp",
            voice_supported=False,
            voice_reason="voice mode is not wired for Vertex in this build",
        )
    if base_url:
        return ProviderInfo(
            name="anthropic-compatible",
            auth_kind="api_key",
            voice_supported=False,
            voice_reason="voice mode currently requires a dedicated Claude.ai-style provider",
        )
    return ProviderInfo(
        name="anthropic",
        auth_kind="api_key",
        voice_supported=False,
        voice_reason="voice mode shell exists, but live voice auth/streaming is not configured in this build",
    )


def auth_status(settings: Settings) -> str:
    """Return a compact auth status string."""
    if settings.api_key:
        return "configured"
    return "missing"


def make_api_client(
    settings: Settings,
    external: SupportsStreamingMessages | None = None,
    *,
    db_kwargs: dict[str, Any] | None = None,
    db_class_path: str | None = None,
) -> SupportsStreamingMessages:
    """Build an OpenAI-compatible API client from settings, or return the external one.

    When *db_kwargs* / *db_class_path* are provided (from the active model
    registration in the DB) they supply ``api_key``, ``base_url``, and the
    provider type — falling back to ``settings`` only when a value is absent.
    """
    if external is not None:
        return external

    from models.clients.openai_compat import OpenAICompatibleClient

    # Resolve from DB-registered model first, then settings
    api_key = (db_kwargs or {}).get("api_key") or settings.resolve_api_key()
    base_url = (db_kwargs or {}).get("base_url") or settings.base_url
    api_format = (db_kwargs or {}).get("api_format") or settings.api_format

    # db_class_path from the model registry also indicates the provider type
    is_anthropic = (
        api_format == "anthropic"
        or (db_class_path or "").endswith("AnthropicClient")
        or db_class_path == "anthropic"
    )

    if is_anthropic:
        from models.clients.anthropic_native import AnthropicClient

        return AnthropicClient(api_key=api_key, base_url=base_url)

    return OpenAICompatibleClient(api_key=api_key, base_url=base_url)
