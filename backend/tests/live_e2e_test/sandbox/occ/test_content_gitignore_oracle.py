"""Phase 1b native probes for the gitignore content oracle."""

from __future__ import annotations

import pytest

from .._harness.native_cases import run_native_case
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


_GITIGNORE_BODY = r"""
from sandbox.occ.content.gitignore_oracle import GitignoreOracle

label = "occ.content_gitignore_oracle"
before = sample_resource()
started = time.perf_counter()
root = _case_root(label) / "repo"
root.mkdir(parents=True)
subprocess.run(["git", "init", "-q", "."], cwd=root, check=True)
(root / ".gitignore").write_text(
    "build/*\n"
    "!build/keep.txt\n"
    "logs/[Ee]rror.[Ll][Oo][Gg]\n",
    encoding="utf-8",
)
(root / "pkg").mkdir()
(root / "pkg" / ".gitignore").write_text("*.tmp\n!important.tmp\n", encoding="utf-8")

oracle = GitignoreOracle(str(root))
paths = [
    "build/out.o",
    "build/keep.txt",
    "pkg/cache.tmp",
    "pkg/important.tmp",
    "logs/error.log",
    "logs/Error.LOG",
    "src/app.py",
]
ignored = {path: oracle.is_ignored(path) for path in paths}
assert ignored == {
    "build/out.o": True,
    "build/keep.txt": False,
    "pkg/cache.tmp": True,
    "pkg/important.tmp": False,
    "logs/error.log": True,
    "logs/Error.LOG": True,
    "src/app.py": False,
}
filtered = oracle.filter_ignored(paths)
assert filtered == {path for path, is_ignored in ignored.items() if is_ignored}

_emit(label, started, before, {
    "ignored": ignored,
    "filtered": sorted(filtered),
    "nested_gitignore": ignored["pkg/cache.tmp"] and not ignored["pkg/important.tmp"],
    "reinclude": not ignored["build/keep.txt"],
    "case_variants": [ignored["logs/error.log"], ignored["logs/Error.LOG"]],
})
"""


async def test_gitignore_oracle_handles_nested_reinclude_and_case_variants(
    native_sandbox: SandboxHandle,
) -> None:
    payload = await run_native_case(
        native_sandbox,
        _GITIGNORE_BODY,
        label="occ.content_gitignore_oracle",
    )
    assert payload["nested_gitignore"] is True
    assert payload["reinclude"] is True
    assert payload["case_variants"] == [True, True]
