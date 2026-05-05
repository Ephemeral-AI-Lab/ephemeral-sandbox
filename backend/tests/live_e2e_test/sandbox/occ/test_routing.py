"""Phase 1b native probes for OCC route decisions."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_ROUTING_BODY = r"""
from sandbox.occ.changeset.prepared import CommitOptions, RouteDecision
from sandbox.occ.changeset.types import OpaqueDirChange, SymlinkChange, WriteChange
from sandbox.occ.orchestrator import OccOrchestrator

class _Gitignore:
    def __init__(self):
        self.ignored = {"dist/app.js", "cache", "ignored-link"}
        self.calls = []
    def is_ignored(self, path):
        self.calls.append(path)
        return path in self.ignored

label = "occ.routing"
before = sample_resource()
started = time.perf_counter()
gitignore = _Gitignore()
router = OccOrchestrator(gitignore)
prepared = router.prepare_sync(
    [
        WriteChange(path="src/app.py", final_content=b"x"),
        WriteChange(path="dist/app.js", final_content=b"x"),
        WriteChange(path=".git/config", final_content=b"x"),
        WriteChange(path="../escape", final_content=b"x"),
        SymlinkChange(path="ignored-link", target="/tmp/data"),
        OpaqueDirChange(path="cache", kept_children=frozenset({"keep"})),
    ],
    snapshot=None,
    options=CommitOptions(),
)
routes = [(group.path, group.route.value) for group in prepared.path_groups]
assert routes == [
    ("src/app.py", RouteDecision.OCC_GATED_MERGE.value),
    ("dist/app.js", RouteDecision.OCC_SKIPPED_MERGE.value),
    (".git/config", RouteDecision.DROP.value),
    ("../escape", RouteDecision.REJECT.value),
    ("ignored-link", RouteDecision.OCC_SKIPPED_MERGE.value),
    ("cache", RouteDecision.OCC_SKIPPED_MERGE.value),
]
assert gitignore.calls == ["src/app.py", "dist/app.js", "ignored-link", "cache"]

_emit(label, started, before, {
    "routes": routes,
    "gitignore_calls": gitignore.calls,
    "drop_message": prepared.path_groups[2].message,
    "reject_message": prepared.path_groups[3].message,
})
"""


async def test_routing_applies_direct_gated_drop_reject_and_override_priority(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _ROUTING_BODY,
        label="occ.routing",
    )
    assert payload["routes"][0] == ["src/app.py", "occ_gated_merge"]
    assert payload["routes"][1] == ["dist/app.js", "occ_skipped_merge"]
    assert payload["routes"][2] == [".git/config", "drop"]
    assert payload["routes"][3] == ["../escape", "reject"]
