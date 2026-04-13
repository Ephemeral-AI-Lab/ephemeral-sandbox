from __future__ import annotations

import logging

from server.logging_config import configure_runtime_logging


def test_configure_runtime_logging_suppresses_noisy_loggers_when_not_verbose():
    logger_names = (
        "httpx",
        "httpcore",
        "engine.core.query",
        "engine.core.streaming_executor",
        "tools.daytona_toolkit.toolkit",
    )
    loggers = [logging.getLogger(name) for name in logger_names]
    original_levels = [logger.level for logger in loggers]

    try:
        for logger in loggers:
            logger.setLevel(logging.INFO)

        configure_runtime_logging(verbose=False)

        assert all(logger.level == logging.WARNING for logger in loggers)
    finally:
        for logger, level in zip(loggers, original_levels, strict=True):
            logger.setLevel(level)


def test_configure_runtime_logging_keeps_existing_levels_in_verbose_mode():
    logger_names = (
        "httpx",
        "engine.core.query",
        "tools.daytona_toolkit.toolkit",
    )
    loggers = [logging.getLogger(name) for name in logger_names]
    original_levels = [logger.level for logger in loggers]

    try:
        for logger in loggers:
            logger.setLevel(logging.INFO)

        configure_runtime_logging(verbose=True)

        assert all(logger.level == logging.INFO for logger in loggers)
    finally:
        for logger, level in zip(loggers, original_levels, strict=True):
            logger.setLevel(level)
