from __future__ import annotations

import logging

from benchmarks.sweevo.__main__ import _configure_benchmark_logging


def test_configure_benchmark_logging_suppresses_warning_and_below() -> None:
    previous_disable_level = logging.root.manager.disable
    try:
        logging.disable(logging.NOTSET)

        _configure_benchmark_logging()

        assert not logging.getLogger("benchmarks.sweevo").isEnabledFor(logging.DEBUG)
        assert not logging.getLogger("benchmarks.sweevo").isEnabledFor(logging.INFO)
        assert not logging.getLogger("benchmarks.sweevo").isEnabledFor(logging.WARNING)
        assert logging.getLogger("benchmarks.sweevo").isEnabledFor(logging.ERROR)
    finally:
        logging.disable(previous_disable_level)
