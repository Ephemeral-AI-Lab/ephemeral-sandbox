"""Models module — LLM providers, clients, registration, and management.

Import from here instead of deep paths:

    from ephemeralos.models import AnthropicApiClient, ModelStore, detect_provider
"""

from ephemeralos.models.types import (
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiStreamEvent,
    ApiTextDeltaEvent,
    SupportsStreamingMessages,
    UsageSnapshot,
)
from ephemeralos.models.errors import (
    AuthenticationFailure,
    EphemeralOSApiError,
    RateLimitFailure,
    RequestFailure,
)
from ephemeralos.models.provider import (
    ProviderInfo,
    auth_status,
    detect_provider,
)
from ephemeralos.models.clients import (
    AnthropicApiClient,
    OpenAICompatibleClient,
)
from ephemeralos.models.db import (
    ModelRegistrationRecord,
    ModelStore,
)
from ephemeralos.models.api import create_models_router

__all__ = [
    # Types & protocol
    "ApiMessageRequest",
    "ApiTextDeltaEvent",
    "ApiMessageCompleteEvent",
    "ApiStreamEvent",
    "SupportsStreamingMessages",
    "UsageSnapshot",
    # Errors
    "EphemeralOSApiError",
    "AuthenticationFailure",
    "RateLimitFailure",
    "RequestFailure",
    # Provider
    "ProviderInfo",
    "detect_provider",
    "auth_status",
    # Clients
    "AnthropicApiClient",
    "OpenAICompatibleClient",
    # DB
    "ModelRegistrationRecord",
    "ModelStore",
    # API
    "create_models_router",
]
