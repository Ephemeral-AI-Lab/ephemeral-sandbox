"""Daytona sandbox exceptions."""


class DaytonaUnavailableError(RuntimeError):
    """Raised when Daytona SDK is not installed or not configured."""


class AsyncDaytonaUnavailableError(RuntimeError):
    """Raised when Async Daytona SDK is not installed or not configured."""
