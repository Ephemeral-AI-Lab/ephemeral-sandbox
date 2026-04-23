"""External trigger module.

The shared ``runner.run()`` loop is used by short-lived helper callers.
"""

from external_trigger.runner import RunResult, run

__all__ = ["RunResult", "run"]
