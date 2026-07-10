"""Reserved `.wh.` catalog wiring: preconditions gate + summary on exit."""

import pytest

from runtime.reserved_paths import helpers


@pytest.fixture(scope="session", autouse=True)
def wh_reserved_preconditions(gateway_up):
    """§1.1 P1-P3 once before any case; hard-fail, never skip."""
    helpers.assert_preconditions_once()


def pytest_terminal_summary(terminalreporter, exitstatus, config):
    path = helpers.finalize_summary(exitstatus=exitstatus)
    terminalreporter.write_sep("-", "reserved .wh. namespace verdict summary")
    terminalreporter.write_line(f"wh-reserved verdict summary: {path}")
