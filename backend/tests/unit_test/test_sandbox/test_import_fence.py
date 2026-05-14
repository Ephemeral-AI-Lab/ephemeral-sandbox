"""Import-fence tests for the sandbox public API cutover."""

from __future__ import annotations

import ast
import importlib
from pathlib import Path


SRC_ROOT = Path(__file__).resolve().parents[2] / "src"
_TOOL_ALLOWED = {
    "sandbox.api",
}
_TOOL_FORBIDDEN_PREFIXES = (
    "sandbox.api.tool",
    "sandbox.provider",
    "sandbox.occ",
    "sandbox.overlay",
    "sandbox.runtime.daemon",
    "sandbox.daytona",
    "sandbox.code_intelligence",
)


def test_agent_sandbox_tools_import_only_public_api_verbs() -> None:
    offenders: list[str] = []
    for module in _python_files(SRC_ROOT / "tools" / "sandbox_toolkit"):
        for imported in _imports(module):
            if not imported.startswith("sandbox."):
                continue
            if imported in _TOOL_ALLOWED:
                continue
            if any(
                imported == prefix or imported.startswith(f"{prefix}.")
                for prefix in _TOOL_FORBIDDEN_PREFIXES
            ):
                offenders.append(
                    f"{module.relative_to(SRC_ROOT)} imports {imported}"
                )
                continue
            offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


def test_non_api_production_code_does_not_import_removed_api_utils() -> None:
    offenders: list[str] = []
    api_root = SRC_ROOT / "sandbox" / "api"
    removed_api_utils = "sandbox.api" + ".utils"
    for module in _python_files(SRC_ROOT):
        if module.is_relative_to(api_root):
            continue
        for imported in _imports(module):
            if imported == removed_api_utils or imported.startswith(
                f"{removed_api_utils}."
            ):
                offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


def test_non_sandbox_production_code_imports_only_public_api() -> None:
    offenders: list[str] = []
    sandbox_root = SRC_ROOT / "sandbox"
    for module in _python_files(SRC_ROOT):
        if module.is_relative_to(sandbox_root):
            continue
        for imported in _imports(module):
            if not imported.startswith("sandbox."):
                continue
            if imported == "sandbox.api":
                continue
            offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


def test_daemon_code_does_not_import_daytona_provider_modules() -> None:
    offenders: list[str] = []
    daemon_root = SRC_ROOT / "sandbox" / "runtime" / "daemon"
    for module in _python_files(daemon_root):
        for imported in _imports(module):
            if imported == "sandbox.provider.daytona" or imported.startswith(
                "sandbox.provider.daytona."
            ):
                offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


# ---------------------------------------------------------------------------
# Provider-agnostic status/control fence (locks the seam from the plan)
# ---------------------------------------------------------------------------


# Allowlisted importer of sandbox.provider.daytona.* outside the daytona
# package itself: the single startup bootstrap call.
_DAYTONA_IMPORT_ALLOWLIST = {
    Path("server/app_factory.py"),
}


def test_no_daytona_imports_outside_provider_package_or_bootstrap() -> None:
    """Daytona is exposed only through the adapter — the provider-agnostic seam."""
    offenders: list[str] = []
    daytona_root = SRC_ROOT / "sandbox" / "provider" / "daytona"
    for module in _python_files(SRC_ROOT):
        rel = module.relative_to(SRC_ROOT)
        if module.is_relative_to(daytona_root):
            continue
        if rel in _DAYTONA_IMPORT_ALLOWLIST:
            continue
        for imported in _imports(module):
            if imported == "sandbox.provider.daytona" or imported.startswith(
                "sandbox.provider.daytona."
            ):
                offenders.append(f"{rel} imports {imported}")

    assert offenders == [], (
        "Modules must not import sandbox.provider.daytona.* outside the "
        f"daytona package: {offenders}"
    )


def test_host_daemon_api_do_not_import_daytona_sdk() -> None:
    """host/, runtime/daemon/, api/ stay free of direct daytona_sdk usage."""
    offenders: list[str] = []
    for root in (
        SRC_ROOT / "sandbox" / "host",
        SRC_ROOT / "sandbox" / "runtime" / "daemon",
        SRC_ROOT / "sandbox" / "api",
    ):
        for module in _python_files(root):
            for imported in _imports(module):
                if imported == "daytona_sdk" or imported.startswith("daytona_sdk."):
                    offenders.append(
                        f"{module.relative_to(SRC_ROOT)} imports {imported}"
                    )

    assert offenders == [], (
        "host/, runtime/daemon/, api/ must not import any daytona SDK module: "
        f"{offenders}"
    )


def test_host_daemon_api_do_not_import_daytona_provider() -> None:
    """The locked seam: host/, runtime/daemon/, and api/ are provider-neutral."""
    offenders: list[str] = []
    for path in (
        SRC_ROOT / "sandbox" / "host",
        SRC_ROOT / "sandbox" / "runtime" / "daemon",
        SRC_ROOT / "sandbox" / "api",
    ):
        if path.is_file():
            modules = [path]
        else:
            modules = _python_files(path)
        for module in modules:
            for imported in _imports(module):
                if imported == "sandbox.provider.daytona" or imported.startswith(
                    "sandbox.provider.daytona."
                ):
                    offenders.append(
                        f"{module.relative_to(SRC_ROOT)} imports {imported}"
                    )

    assert offenders == [], (
        "host/, runtime/daemon/, and api/ must not import "
        f"sandbox.provider.daytona.*: {offenders}"
    )


def test_removed_api_compatibility_modules_stay_absent() -> None:
    for module in (
        "sandbox.api.facade",
        "sandbox.api.status",
        "sandbox.api.tool.read",
        "sandbox.api.tool.write",
        "sandbox.api.tool.edit",
        "sandbox.api.tool.shell",
        "sandbox.api.tool.raw_exec",
    ):
        try:
            importlib.import_module(module)
        except ModuleNotFoundError as exc:
            # exc.name may be the leaf (the parent package still exists as a
            # namespace) or an ancestor (the parent dir was also removed in
            # the sandbox-reframe W0 skeleton purge). Both are valid "module
            # is absent" outcomes.
            assert exc.name == module or module.startswith(f"{exc.name}.")
        else:
            raise AssertionError(f"{module} should not be importable")


def test_provider_package_does_not_import_host_layer() -> None:
    """Providers own adapters; host orchestration sits above them."""
    offenders: list[str] = []
    for module in _python_files(SRC_ROOT / "sandbox" / "provider"):
        for imported in _imports(module):
            if imported == "sandbox.host" or imported.startswith("sandbox.host."):
                offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


def test_occ_policy_modules_depend_on_layer_stack_ports_not_manager() -> None:
    """OCC internals use role protocols instead of the concrete storage facade."""
    offenders: list[str] = []
    occ_root = SRC_ROOT / "sandbox" / "occ"
    allowed = {
        occ_root / "ports.py",
    }
    forbidden = (
        "sandbox.layer_stack.manager",
        "sandbox.layer_stack.view.merged",
        "sandbox.layer_stack.layer.publisher",
        "sandbox.layer_stack.lease.registry",
        "sandbox.runtime.daemon.service.workspace_server",
    )
    for module in _python_files(occ_root):
        if module in allowed:
            continue
        for imported in _imports(module):
            if imported in forbidden or any(
                imported.startswith(f"{prefix}.") for prefix in forbidden
            ):
                offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


def test_layer_stack_package_has_no_occ_command_exec_or_git_policy_imports() -> None:
    offenders: list[str] = []
    forbidden = (
        "sandbox.occ",
        "sandbox.command_exec",
        "sandbox.runtime.daemon.service.workspace_binding",
        "pathspec",
    )
    for module in _python_files(SRC_ROOT / "sandbox" / "layer_stack"):
        for imported in _imports(module):
            if imported in forbidden or any(
                imported.startswith(f"{prefix}.") for prefix in forbidden
            ):
                offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


def test_removed_lowerdir_cache_modules_stay_absent() -> None:
    for module in (
        "sandbox.layer_stack.snapshot_cache",
        "sandbox.layer_stack.metrics",
    ):
        try:
            importlib.import_module(module)
        except ModuleNotFoundError as exc:
            assert exc.name == module
        else:
            raise AssertionError(f"{module} should not be importable")


def test_removed_top_level_contract_module_stays_absent() -> None:
    try:
        importlib.import_module("sandbox.contract")
    except ModuleNotFoundError as exc:
        assert exc.name == "sandbox.contract"
    else:
        raise AssertionError("sandbox.contract should not be importable")


def test_command_exec_imports_only_client_protocol_boundaries() -> None:
    offenders: list[str] = []
    command_exec_root = SRC_ROOT / "sandbox" / "command_exec"
    forbidden = (
        "sandbox.layer_stack",
        "sandbox.occ.service",
        "sandbox.occ.stage.transaction",
        "sandbox.occ.content.gitignore_oracle",
        "sandbox.occ.stage.direct",
        "sandbox.occ.stage.gated",
        "sandbox.occ.router",
        "sandbox.occ.content.hashing",
        "sandbox.runtime.daemon.service.workspace_server",
    )
    for module in _python_files(command_exec_root):
        for imported in _imports(module):
            if imported in forbidden or any(
                imported.startswith(f"{prefix}.") for prefix in forbidden
            ):
                offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


def test_internal_sandbox_layers_do_not_import_public_api() -> None:
    """Internal layers stay below the public facade in the dependency DAG."""
    offenders: list[str] = []
    for root in (
        SRC_ROOT / "sandbox" / "host",
        SRC_ROOT / "sandbox" / "runtime" / "daemon",
        SRC_ROOT / "sandbox" / "provider",
        SRC_ROOT / "sandbox" / "occ",
        SRC_ROOT / "sandbox" / "overlay",
        SRC_ROOT / "sandbox" / "layer_stack",
        SRC_ROOT / "sandbox" / "command_exec",
    ):
        for module in _python_files(root):
            for imported in _imports(module):
                if imported == "sandbox.api" or imported.startswith("sandbox.api."):
                    offenders.append(f"{module.relative_to(SRC_ROOT)} imports {imported}")

    assert offenders == []


def _python_files(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*.py") if "__pycache__" not in path.parts)


def _imports(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            names.add(node.module)
    return names
