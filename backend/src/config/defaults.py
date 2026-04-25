"""Centralized default values for EphemeralOS.

This module contains all hardcoded constants, limits, and magic values
that should be configurable. Values here serve as defaults that can be
overridden via Settings or environment variables.
"""

from __future__ import annotations

# ---------------------------------------------------------------------------
# Provider/Retry Defaults
# ---------------------------------------------------------------------------

DEFAULT_MAX_RETRIES: int = 3
DEFAULT_BASE_DELAY: float = 1.0
DEFAULT_MAX_DELAY: float = 30.0
DEFAULT_RETRY_STATUS_CODES: frozenset[int] = frozenset({429, 500, 502, 503, 529})

# ---------------------------------------------------------------------------
# Database Defaults
# ---------------------------------------------------------------------------

DEFAULT_DATABASE_POOL_SIZE: int = 5
DEFAULT_DATABASE_MAX_OVERFLOW: int = 10

# ---------------------------------------------------------------------------
# Sandbox Defaults
# ---------------------------------------------------------------------------

DEFAULT_SANDBOX_CI_ROOT: str = "/home/daytona"
