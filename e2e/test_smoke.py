"""One-test check: gateway up and a structured list_sandboxes response.

Run with ``python3 -m pytest test_smoke.py``. The ``smoke`` marker also selects
the broader cross-family smoke tier.
"""

import pytest

from core.cli import is_error
from manager.management import helpers as mgmt


@pytest.mark.smoke
def test_gateway_responds_with_sandbox_list():
    result = mgmt.list_sandboxes()
    assert not is_error(result), result
    assert isinstance(result.get("sandboxes"), list)
