"""Unit tests for sandbox.plugin.install."""

from __future__ import annotations

import asyncio
import shlex
from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path

import pytest

from plugins.core.manifest import parse_plugin_manifest
from sandbox.plugin import install as install_mod
from sandbox.plugin.install import (
    PluginInstallError,
    ensure_installed,
    plugin_install_dir,
    plugin_marker_path,
)


@pytest.fixture(autouse=True)
def _clear_install_caches(tmp_path: Path) -> Iterator[None]:
    install_mod._locks.clear()
    # Tests stage plugin source trees under tmp_path and expect setup.sh to
    # run; opt them into the trusted-setup allowlist so the C1 gate doesn't
    # refuse the test fixture's source_dir.
    resolved = tmp_path.resolve()
    install_mod._TRUSTED_SETUP_ROOTS.add(resolved)
    yield
    install_mod._locks.clear()
    install_mod._TRUSTED_SETUP_ROOTS.discard(resolved)


def _seed_demo_plugin(tmp_path: Path, *, with_runtime: bool = True) -> Path:
    plugin_dir = tmp_path / "demo"
    plugin_dir.mkdir()
    runtime_line = "runtime: runtime/server.py\n" if with_runtime else ""
    (plugin_dir / "plugin.md").write_text(
        "---\nname: demo\ndescription: demo\ntools:\n"
        "  - name: demo.run\n    module: tools/run.py\nsetup: setup.sh\n"
        f"{runtime_line}---\n",
        encoding="utf-8",
    )
    (plugin_dir / "tools").mkdir()
    (plugin_dir / "tools" / "run.py").write_text("x = 1\n", encoding="utf-8")
    (plugin_dir / "setup.sh").write_text(
        '#!/bin/sh\necho "installing"\n', encoding="utf-8"
    )
    if with_runtime:
        (plugin_dir / "runtime").mkdir()
        (plugin_dir / "runtime" / "server.py").write_text(
            "def hello():\n    return 1\n", encoding="utf-8"
        )
    return plugin_dir


@dataclass
class _FakeResult:
    exit_code: int = 0
    stdout: str = ""
    stderr: str = ""


@dataclass
class _FakeExec:
    """Mock provider exec_fn that records every command and returns scripted exit codes."""

    marker_present: bool = False
    setup_exit_code: int = 0
    calls: list[str] = field(default_factory=list)

    async def __call__(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> _FakeResult:
        del sandbox_id, cwd, timeout
        self.calls.append(command)
        if command.startswith("test -f"):
            return _FakeResult(exit_code=0 if self.marker_present else 1)
        if "setup.sh" in command and "EOS_PLUGIN_DIR" in command:
            return _FakeResult(
                exit_code=self.setup_exit_code,
                stderr="setup boom" if self.setup_exit_code != 0 else "",
            )
        return _FakeResult(exit_code=0)


def _seed_lsp_plugin(tmp_path: Path) -> Path:
    plugin_dir = tmp_path / "lsp"
    plugin_dir.mkdir()
    (plugin_dir / "plugin.md").write_text(
        "---\nname: lsp\ndescription: lsp plugin\ntools:\n"
        "  - name: lsp.hover\n    module: tools/hover.py\n"
        "setup: setup.sh\nruntime: runtime/server.py\n---\n",
        encoding="utf-8",
    )
    (plugin_dir / "tools").mkdir()
    (plugin_dir / "tools" / "hover.py").write_text("x = 1\n", encoding="utf-8")
    (plugin_dir / "runtime").mkdir()
    (plugin_dir / "runtime" / "server.py").write_text("x = 1\n", encoding="utf-8")
    (plugin_dir / "setup.sh").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    return plugin_dir


def test_marker_hit_short_circuits(tmp_path: Path) -> None:
    plugin_dir = _seed_demo_plugin(tmp_path)
    manifest = parse_plugin_manifest(plugin_dir)

    fake = _FakeExec(marker_present=True)
    digest = asyncio.run(
        ensure_installed("sb-1", manifest, exec_fn=fake)
    )

    assert digest
    # Only the marker check ran; no upload / extract / setup.
    assert any(c.startswith("test -f") for c in fake.calls)
    assert not any("base64 -d" in c for c in fake.calls)
    assert not any("setup.sh" in c and "EOS_PLUGIN_DIR" in c for c in fake.calls)


def test_marker_miss_uploads_and_runs_setup(tmp_path: Path) -> None:
    plugin_dir = _seed_demo_plugin(tmp_path)
    manifest = parse_plugin_manifest(plugin_dir)

    fake = _FakeExec(marker_present=False)
    digest = asyncio.run(
        ensure_installed("sb-1", manifest, exec_fn=fake)
    )

    install_dir = plugin_install_dir("demo")
    marker = plugin_marker_path("demo", digest)

    # Setup ran with EOS_PLUGIN_DIR pointing at the install dir.
    assert any(
        f"export EOS_PLUGIN_DIR={shlex.quote(install_dir)}" in c
        for c in fake.calls
    )
    # Marker was written with the digest.
    assert any(
        f"printf %s {shlex.quote(digest)} > {shlex.quote(marker)}" in c
        for c in fake.calls
    )
    # At least one base64 chunk write happened.
    assert any("base64 -d" in c for c in fake.calls)
    assert any(".staging-" in c for c in fake.calls)


def test_install_does_not_use_sticky_marker_cache(tmp_path: Path) -> None:
    plugin_dir = _seed_demo_plugin(tmp_path)
    manifest = parse_plugin_manifest(plugin_dir)
    fake = _FakeExec(marker_present=False)

    asyncio.run(ensure_installed("sb-1", manifest, exec_fn=fake))
    asyncio.run(ensure_installed("sb-1", manifest, exec_fn=fake))

    setup_runs = [
        command
        for command in fake.calls
        if "setup.sh" in command and "EOS_PLUGIN_DIR" in command
    ]
    assert len(setup_runs) == 2


def test_setup_failure_surfaces_clear_error(tmp_path: Path) -> None:
    plugin_dir = _seed_demo_plugin(tmp_path)
    manifest = parse_plugin_manifest(plugin_dir)

    fake = _FakeExec(marker_present=False, setup_exit_code=2)
    with pytest.raises(PluginInstallError, match="setup.sh failed"):
        asyncio.run(ensure_installed("sb-1", manifest, exec_fn=fake))


def test_concurrent_first_calls_share_one_setup(tmp_path: Path) -> None:
    plugin_dir = _seed_demo_plugin(tmp_path)
    manifest = parse_plugin_manifest(plugin_dir)

    state: dict[str, int] = {"setup_runs": 0}

    @dataclass
    class _CountingExec(_FakeExec):
        async def __call__(
            self,
            sandbox_id: str,
            command: str,
            *,
            cwd: str | None = None,
            timeout: int | None = None,
        ) -> _FakeResult:
            if "setup.sh" in command and "EOS_PLUGIN_DIR" in command:
                state["setup_runs"] += 1
            # Marker stays present after the first install completes.
            if command.startswith("test -f") and state["setup_runs"] >= 1:
                self.marker_present = True
            return await _FakeExec.__call__(
                self,
                sandbox_id,
                command,
                cwd=cwd,
                timeout=timeout,
            )

    fake = _CountingExec(marker_present=False)

    async def run() -> None:
        await asyncio.gather(
            ensure_installed("sb-1", manifest, exec_fn=fake),
            ensure_installed("sb-1", manifest, exec_fn=fake),
            ensure_installed("sb-1", manifest, exec_fn=fake),
        )

    asyncio.run(run())
    assert state["setup_runs"] == 1


def test_hash_changes_when_source_file_changes(tmp_path: Path) -> None:
    plugin_dir = _seed_demo_plugin(tmp_path)
    manifest_v1 = parse_plugin_manifest(plugin_dir)
    digest_v1 = install_mod._bundle_hash(manifest_v1)

    # Mutate one source file.
    (plugin_dir / "tools" / "run.py").write_text("x = 2\n", encoding="utf-8")
    manifest_v2 = parse_plugin_manifest(plugin_dir)
    digest_v2 = install_mod._bundle_hash(manifest_v2)

    assert digest_v1 != digest_v2


def test_install_free_plugin_skips_setup(tmp_path: Path) -> None:
    """A manifest without setup must not invoke setup.sh."""
    plugin_dir = tmp_path / "lite"
    plugin_dir.mkdir()
    (plugin_dir / "plugin.md").write_text(
        "---\nname: lite\ndescription: lite\ntools:\n"
        "  - name: lite.run\n    module: tools/run.py\n---\n",
        encoding="utf-8",
    )
    (plugin_dir / "tools").mkdir()
    (plugin_dir / "tools" / "run.py").write_text("x=1\n", encoding="utf-8")
    manifest = parse_plugin_manifest(plugin_dir)

    fake = _FakeExec(marker_present=False)
    asyncio.run(ensure_installed("sb-1", manifest, exec_fn=fake))

    # No setup invocation in the call log.
    assert not any(
        "setup.sh" in c and "EOS_PLUGIN_DIR" in c for c in fake.calls
    )


def test_lsp_install_runs_setup_without_host_assets(tmp_path: Path) -> None:
    plugin_dir = _seed_lsp_plugin(tmp_path)
    manifest = parse_plugin_manifest(plugin_dir)

    fake = _FakeExec(marker_present=False)
    asyncio.run(ensure_installed("sb-1", manifest, exec_fn=fake))

    install_dir = plugin_install_dir("lsp")
    assert any(
        f"export EOS_PLUGIN_DIR={shlex.quote(install_dir)}" in command
        for command in fake.calls
    )
    assert not any(
        "ARCHIVE" in command or "PACKAGE" in command
        for command in fake.calls
    )
    assert not any(
        command == "uname -m" or "/vendor/" in command
        for command in fake.calls
    )
