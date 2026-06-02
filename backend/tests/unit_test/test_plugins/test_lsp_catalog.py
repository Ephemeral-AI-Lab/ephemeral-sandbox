"""Unit tests for the LSP plugin catalog (manifest + tool files)."""

from __future__ import annotations

import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest

from plugins.core import loader as loader_mod
from plugins.core.discovery import discover_plugins
from plugins.core.loader import register_plugin_tools
from plugins.core.manifest import parse_plugin_manifest


_LSP_DIR = (
    Path(__file__).resolve().parents[3]
    / "src"
    / "plugins"
    / "catalog"
    / "lsp"
)


@pytest.fixture(autouse=True)
def _isolate_loader() -> Iterator[None]:
    loader_mod._LOAD_CACHE.clear()
    pre = {
        name for name in sys.modules if name.startswith("plugins.catalog.")
    }
    yield
    loader_mod._LOAD_CACHE.clear()
    for name in [
        n
        for n in list(sys.modules)
        if n.startswith("plugins.catalog.") and n not in pre
    ]:
        sys.modules.pop(name, None)


def test_lsp_manifest_parses() -> None:
    manifest = parse_plugin_manifest(_LSP_DIR)
    assert manifest.name == "lsp"
    tool_names = sorted(t.name for t in manifest.tools)
    assert tool_names == [
        "lsp.apply_code_action",
        "lsp.apply_workspace_edit",
        "lsp.code_actions",
        "lsp.diagnostics",
        "lsp.find_definitions",
        "lsp.find_references",
        "lsp.format",
        "lsp.hover",
        "lsp.query_symbols",
        "lsp.rename",
    ]
    assert manifest.setup is not None
    assert manifest.setup.name == "setup.sh"
    assert manifest.runtime is not None
    assert manifest.runtime.name == "server.py"


def test_lsp_discovery_picks_up_the_plugin() -> None:
    catalog_dir = _LSP_DIR.parent
    plugins = discover_plugins(catalog_dir)
    assert any(m.name == "lsp" for m in plugins)


def test_register_plugin_tools_yields_lsp_tools() -> None:
    catalog_dir = _LSP_DIR.parent
    tools = register_plugin_tools(catalog_dir)
    lsp_tools = sorted(t.name for t in tools if t.name.startswith("lsp."))
    assert lsp_tools == [
        "lsp.apply_code_action",
        "lsp.apply_workspace_edit",
        "lsp.code_actions",
        "lsp.diagnostics",
        "lsp.find_definitions",
        "lsp.find_references",
        "lsp.format",
        "lsp.hover",
        "lsp.query_symbols",
        "lsp.rename",
    ]


def test_each_lsp_tool_creatable_via_factory() -> None:
    """Round-trip: register_plugin_tools → tools._framework.factory → create_tool."""
    from tools._framework.factory import (
        ToolFactoryContext,
        create_tool,
    )

    # ``create_tool`` triggers the production builtin-registration path which
    # itself walks the plugin catalog and registers each tool. The round-trip
    # therefore is: production startup → factory → tool instance. We do NOT
    # double-register manually here — that collides with the builtin
    # registration when ``_ensure_builtins_registered`` runs (see factory.py
    # ``_register_many`` rejecting duplicates without ``override=True``).
    for name in (
        "lsp.apply_code_action",
        "lsp.apply_workspace_edit",
        "lsp.code_actions",
        "lsp.hover",
        "lsp.find_definitions",
        "lsp.find_references",
        "lsp.diagnostics",
        "lsp.query_symbols",
        "lsp.rename",
        "lsp.format",
    ):
        instance = create_tool(name, ToolFactoryContext())
        assert instance.name == name


def test_lsp_tool_modules_do_not_import_sandbox_internals() -> None:
    """Plugin tools must only import sandbox.* through sandbox.ephemeral_workspace.plugin."""
    forbidden_prefixes = (
        "sandbox.runtime",
        "sandbox.layer_stack",
        "sandbox.host",
        "sandbox.provider",
        "sandbox.api",  # tools should not import sandbox.api directly
    )
    for path in (_LSP_DIR / "tools").glob("*.py"):
        if path.name == "__init__.py":
            continue
        text = path.read_text(encoding="utf-8")
        for prefix in forbidden_prefixes:
            assert (
                f"from {prefix}" not in text and f"import {prefix}" not in text
            ), f"{path.name} imports forbidden {prefix}"


def test_lsp_setup_script_self_locates_and_installs_pyright(
    tmp_path: Path,
) -> None:
    plugin_dir = tmp_path / "lsp"
    plugin_dir.mkdir()
    (plugin_dir / "setup.sh").write_text(
        (_LSP_DIR / "setup.sh").read_text(encoding="utf-8"),
        encoding="utf-8",
    )

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    package_dir = tmp_path / "package"
    package_dir.mkdir()
    fake_node_home = package_dir / "node"
    fake_node_home.mkdir()
    (package_dir / "node.tar.xz").write_bytes(b"node archive")
    (package_dir / "pyright.tgz").write_bytes(b"pyright archive")
    log_path = tmp_path / "npm.log"
    (fake_bin / "node").write_text(
        """#!/usr/bin/env bash
set -eu
printf 'v22.13.1\n'
""",
        encoding="utf-8",
    )
    (fake_bin / "node").chmod(0o755)
    (fake_bin / "npm").write_text(
        """#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "$PYRIGHT_SETUP_LOG"
if [ "${1:-}" = "-v" ]; then
    printf '10.9.2\n'
    exit 0
fi
if [ "${1:-}" = "config" ] && [ "${2:-}" = "set" ]; then
    exit 0
fi
if [ "${1:-}" = "install" ]; then
    test "${2:-}" = "-g"
    test "${3:-}" = "--offline"
    test "${4:-}" = "--cache"
    test "${5:-}" = "$EOS_PLUGIN_PACKAGE_DIR/npm-cache"
    test "${6:-}" = "--omit=optional"
    test "${7:-}" = "$EOS_PLUGIN_PACKAGE_DIR/pyright.tgz"
    mkdir -p "$EOS_NODE_HOME/bin"
    printf '#!/usr/bin/env sh\nprintf "pyright 1.1.409\\n"\n' > "$EOS_NODE_HOME/bin/pyright"
    printf '#!/usr/bin/env sh\nexit 0\n' > "$EOS_NODE_HOME/bin/pyright-langserver"
    chmod +x "$EOS_NODE_HOME/bin/pyright" "$EOS_NODE_HOME/bin/pyright-langserver"
    exit 0
fi
printf 'unexpected npm args: %s\n' "$*" >&2
exit 99
""",
        encoding="utf-8",
    )
    (fake_bin / "npm").chmod(0o755)
    (fake_bin / "tar").write_text(
        """#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "$PYRIGHT_SETUP_LOG"
exit 0
""",
        encoding="utf-8",
    )
    (fake_bin / "tar").chmod(0o755)
    (fake_bin / "curl").write_text("#!/usr/bin/env bash\nexit 99\n", encoding="utf-8")
    (fake_bin / "curl").chmod(0o755)

    env = {
        "PATH": f"{fake_bin}:/usr/bin:/bin",
        "EOS_NODE_HOME": str(fake_node_home),
        "EOS_PLUGIN_PACKAGE_DIR": str(package_dir),
        "PYRIGHT_SETUP_LOG": str(log_path),
    }
    completed = subprocess.run(
        ["bash", str(plugin_dir / "setup.sh")],
        cwd=plugin_dir,
        env=env,
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )

    assert completed.returncode == 0, completed.stderr
    assert (plugin_dir / ".pyright_installed").is_file()
    npm_calls = log_path.read_text(encoding="utf-8").splitlines()
    assert "config set prefix " + str(fake_node_home) in npm_calls
    assert (
        "install -g --offline --cache "
        f"{package_dir}/npm-cache --omit=optional {package_dir}/pyright.tgz"
    ) in npm_calls


def test_lsp_setup_script_marker_short_circuits_when_pyright_exists(
    tmp_path: Path,
) -> None:
    plugin_dir = tmp_path / "lsp"
    plugin_dir.mkdir()
    (plugin_dir / "setup.sh").write_text(
        (_LSP_DIR / "setup.sh").read_text(encoding="utf-8"),
        encoding="utf-8",
    )

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    fake_node_home = tmp_path / "node"
    (fake_node_home / "bin").mkdir(parents=True)
    (fake_node_home / "bin" / "pyright-langserver").write_text(
        "#!/usr/bin/env sh\nexit 0\n",
        encoding="utf-8",
    )
    (fake_node_home / "bin" / "pyright-langserver").chmod(0o755)
    (plugin_dir / ".pyright_installed").touch()
    (fake_bin / "curl").write_text(
        """#!/usr/bin/env bash
exit 99
""",
        encoding="utf-8",
    )
    (fake_bin / "curl").chmod(0o755)
    (fake_bin / "npm").write_text(
        """#!/usr/bin/env bash
exit 99
""",
        encoding="utf-8",
    )
    (fake_bin / "npm").chmod(0o755)
    (fake_bin / "node").write_text(
        """#!/usr/bin/env bash
exit 99
""",
        encoding="utf-8",
    )
    (fake_bin / "node").chmod(0o755)

    env = {
        "PATH": f"{fake_bin}:/usr/bin:/bin",
        "EOS_NODE_HOME": str(fake_node_home),
    }
    completed = subprocess.run(
        ["bash", str(plugin_dir / "setup.sh")],
        cwd=plugin_dir,
        env=env,
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )

    assert completed.returncode == 0, completed.stderr
    assert (plugin_dir / ".pyright_installed").is_file()
