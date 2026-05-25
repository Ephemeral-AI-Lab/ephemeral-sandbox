"""Single-path shell captures opt out of cross-path atomicity.

When the workspace upperdir capture from a guarded shell call yields
exactly one distinct path, ``CommitOptions.atomic`` is set to ``False``
so ``CommitQueue._disjoint_batches`` can coalesce concurrent shell
commits into a single revalidate-and-publish round-trip. Multi-path
captures keep ``atomic=True`` to preserve all-or-nothing semantics for
real workloads (e.g. ``make build``).
"""

from __future__ import annotations

from tests.occ_change_helpers import write_change

import asyncio
from dataclasses import dataclass
from typing import Any

import pytest

import sandbox.ephemeral_workspace.workspace_publish as publishing
import sandbox.ephemeral_workspace.pipeline as pipeline_mod
from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline
from sandbox._shared.command_exec_contract import CommandExecRequest
from sandbox.occ.changeset import CommitOptions
from sandbox.occ.changeset import ChangesetResult, WriteChange
from sandbox.overlay.path_change import OverlayPathChange

_CAPTURED_PATHS: list[str] = []


@dataclass
class _Manifest:
    version: int = 1


class _StubOccClient:
    """Captures the ``CommitOptions`` passed to ``apply_changeset``."""

    def __init__(self) -> None:
        self.captured_options: list[CommitOptions] = []

    async def apply_changeset(
        self,
        typed_changes: Any,
        *,
        snapshot: Any = None,
        options: CommitOptions | None = None,
        workspace_ref: str | None = None,
        run_maintenance: bool = True,
    ) -> ChangesetResult:
        del typed_changes, snapshot, workspace_ref, run_maintenance
        assert options is not None
        self.captured_options.append(options)
        return ChangesetResult(
            files=(),
            timings={},
            published_manifest_version=1,
        )

    async def run_maintenance_after_publish(
        self,
        result: ChangesetResult,
        *,
        workspace_ref: str | None = None,
    ) -> dict[str, float]:
        del result, workspace_ref
        return {}


def _request() -> CommandExecRequest:
    return CommandExecRequest(
        invocation_id="atomic-by-path-test",
        workspace_ref="/tmp/eos-test-atomic",
        workspace_root="/testbed",
        command=("true",),
        agent_id="t",
        description="atomic-by-path",
    )


@pytest.fixture(autouse=True)
def _patch_workspace_to_occ(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path,
) -> None:
    """Bypass on-disk content readers; emit one ``WriteChange`` per path."""

    def fake_walk_upperdir(*args: Any, **kwargs: Any) -> list[OverlayPathChange]:
        del args, kwargs
        return [
            OverlayPathChange(
                path=path,
                kind="write",
                content_path="/tmp/unused",
                final_hash="hash",
            )
            for path in _CAPTURED_PATHS
        ]

    def fake(path_changes: Any, **_: Any) -> tuple[WriteChange, ...]:
        return tuple(
            write_change(
                path=change.path,
                final_content=b"x",
                source="overlay_capture",
            )
            for change in path_changes
        )

    monkeypatch.setattr(publishing, "walk_upperdir", fake_walk_upperdir)
    monkeypatch.setattr(publishing, "overlay_path_changes_to_occ_changes", fake)
    writable_root = tmp_path / "overlay-writable"
    writable_root.mkdir()
    monkeypatch.setattr(pipeline_mod, "overlay_writable_root", lambda: writable_root)


def _apply(client: _StubOccClient, paths: list[str]) -> None:
    _CAPTURED_PATHS[:] = paths
    overlay = EphemeralPipeline(
        occ_client=client,  # type: ignore[arg-type]
        workspace_ref=_request().workspace_ref,
    )
    asyncio.run(
        overlay.publish_cycle(
            request=_request(),
            upperdir="/tmp/unused-upperdir",
            snapshot=_Manifest(),
        )
    )


def test_single_path_capture_passes_atomic_false() -> None:
    client = _StubOccClient()
    _apply(client, ["only/file.txt"])
    assert len(client.captured_options) == 1
    assert client.captured_options[0].atomic is False


def test_multi_path_capture_keeps_atomic_true() -> None:
    client = _StubOccClient()
    _apply(client, ["build/out.o", "build/out.so"])
    assert len(client.captured_options) == 1
    assert client.captured_options[0].atomic is True


def test_repeated_writes_to_one_path_are_single_path() -> None:
    """Two changes touching the same path → one distinct path → atomic=False."""
    client = _StubOccClient()
    _apply(client, ["dup/file.txt", "dup/file.txt"])
    assert client.captured_options[0].atomic is False
