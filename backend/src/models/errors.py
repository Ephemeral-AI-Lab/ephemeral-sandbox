"""API error types for EphemeralOS."""

from __future__ import annotations


class EphemeralOSApiError(RuntimeError):
    """Base class for upstream API failures."""


class AuthenticationFailure(EphemeralOSApiError):
    """Raised when the upstream service rejects the provided credentials."""


class RateLimitFailure(EphemeralOSApiError):
    """Raised when the upstream service rejects the request due to rate limits."""


class RequestFailure(EphemeralOSApiError):
    """Raised for generic request or transport failures."""
