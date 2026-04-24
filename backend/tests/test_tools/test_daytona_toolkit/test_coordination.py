"""Tests for team.core.scope scope-extraction helpers."""

from team.core.scope import scope_paths_from_payload


def test_scope_paths_from_payload_includes_owned_files_and_verify_paths():
    payload = {
        "owned_files": ["pkg/core.py", "pkg/core.py"],
        "verify": [
            "pytest -q pkg/tests/test_core.py::test_one",
            "python -m pytest pkg/tests/test_extra.py -q",
            "pytest -q",
        ],
    }

    scope_paths = scope_paths_from_payload(payload)

    assert "pkg/core.py" in scope_paths
    assert "pkg/tests/test_core.py" in scope_paths
    assert "pkg/tests/test_extra.py" in scope_paths

