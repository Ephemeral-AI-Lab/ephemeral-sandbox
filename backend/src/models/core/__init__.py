"""Core model types, errors, and provider abstractions.

Re-exports from submodules for convenient imports.
"""

from models.core.types import (
    ApiCancelEvent,
    ApiMessageCompleteEvent,
    ApiMessageRequest,
    ApiStreamEvent,
    ApiTextDeltaEvent,
    ApiThinkingDeltaEvent,
    ApiToolUseDeltaEvent,
    SupportsStreamingMessages,
    UsageSnapshot,
)
from models.core.errors import (
    AuthenticationFailure,
    EphemeralOSApiError,
    RateLimitFailure,
    RequestFailure,
)
from models.core.provider import (
    ProviderInfo,
    auth_status,
    detect_provider,
    make_api_client,
)

__all__ = [
    # Types
    "ApiCancelEvent",
    "ApiMessageCompleteEvent",
    "ApiMessageRequest",
    "ApiStreamEvent",
    "ApiTextDeltaEvent",
    "ApiThinkingDeltaEvent",
    "ApiToolUseDeltaEvent",
    "SupportsStreamingMessages",
    "UsageSnapshot",
    # Errors
    "EphemeralOSApiError",
    "AuthenticationFailure",
    "RateLimitFailure",
    "RequestFailure",
    # Provider
    "ProviderInfo",
    "auth_status",
    "detect_provider",
    "make_api_client",
]
