"""Live aggregate and sandbox-scoped observability through the public CLI."""

import pytest

from core.cli import cli, is_error


@pytest.mark.smoke
def test_aggregate_snapshot_includes_ready_sandbox(sandbox):
    result = cli("observability", "snapshot")

    assert not is_error(result), result
    snapshots = result.get("sandboxes")
    assert isinstance(snapshots, list), result
    matching = [item for item in snapshots if item.get("sandbox_id") == sandbox]
    assert len(matching) == 1, result
    assert matching[0]["lifecycle_state"] == "ready", matching[0]
    assert matching[0]["availability"] in {"available", "partial"}, matching[0]


@pytest.mark.smoke
def test_scoped_snapshot_returns_selected_sandbox(sandbox):
    result = cli("observability", "snapshot", "--sandbox-id", sandbox)

    assert not is_error(result), result
    assert result["sandbox_id"] == sandbox, result
    assert result["lifecycle_state"] == "ready", result
    assert result["availability"] in {"available", "partial"}, result
