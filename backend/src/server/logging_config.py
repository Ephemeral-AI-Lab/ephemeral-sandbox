from __future__ import annotations

import logging


def configure_runtime_logging(*, verbose: bool) -> None:
    """Clamp noisy library/runtime loggers unless verbose output is enabled."""
    if verbose:
        return

    for name in (
        "httpx",
        "httpcore",
        "engine.core.query",
        "engine.core.streaming_executor",
        "tools.daytona_toolkit.registry",
    ):
        logging.getLogger(name).setLevel(logging.WARNING)
