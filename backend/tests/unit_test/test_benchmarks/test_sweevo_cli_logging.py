from __future__ import annotations

import logging

from task_center_runner.benchmarks.sweevo.__main__ import _build_parser, _configure_logging


def test_configure_logging_suppresses_warning_and_below() -> None:
    previous_disable_level = logging.root.manager.disable
    try:
        logging.disable(logging.NOTSET)
        _configure_logging()
        assert not logging.getLogger("task_center_runner.benchmarks.sweevo").isEnabledFor(logging.DEBUG)
        assert not logging.getLogger("task_center_runner.benchmarks.sweevo").isEnabledFor(logging.INFO)
        assert not logging.getLogger("task_center_runner.benchmarks.sweevo").isEnabledFor(logging.WARNING)
        assert logging.getLogger("task_center_runner.benchmarks.sweevo").isEnabledFor(logging.ERROR)
    finally:
        logging.disable(previous_disable_level)


def test_sweevo_cli_requires_instance_id() -> None:
    parser = _build_parser()
    args = parser.parse_args(["--instance-id", "dask__dask_2023.3.2_2023.4.0"])
    assert args.instance_id == "dask__dask_2023.3.2_2023.4.0"
