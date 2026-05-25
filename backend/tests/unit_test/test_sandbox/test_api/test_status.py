"""Unit tests for public sandbox lifecycle, discovery, and URL modules."""

from __future__ import annotations

import threading
from unittest.mock import MagicMock

import pytest


@pytest.fixture(autouse=True)
def _isolate_registry(monkeypatch: pytest.MonkeyPatch) -> None:
    from sandbox.provider import registry as reg

    monkeypatch.setattr(reg, "_ADAPTERS", {}, raising=False)
    monkeypatch.setattr(reg, "_DEFAULT", None, raising=False)
    monkeypatch.setattr(reg, "_LOCK", threading.Lock(), raising=False)


def _stub_provider() -> MagicMock:
    provider = MagicMock(name="provider")
    provider.create.return_value = {
        "id": "sb-1",
        "name": "demo",
        "state": "started",
        "project_dir": "/workspace/demo",
    }
    provider.start.return_value = {
        "id": "sb-1",
        "state": "started",
        "project_dir": "/workspace/demo",
    }
    provider.stop.return_value = {"id": "sb-1", "state": "stopped"}
    provider.get.return_value = {"id": "sb-1", "state": "started"}
    provider.list.return_value = [{"id": "sb-1"}]
    provider.get_health.return_value = {"available": True}
    provider.list_snapshots.return_value = [{"name": "snap"}]
    provider.get_signed_preview_url.return_value = {"url": "https://"}
    provider.get_build_logs_url.return_value = "https://logs"
    provider.set_labels.return_value = {
        "id": "sb-1",
        "labels": {"project_dir": "/testbed"},
    }
    return provider


def test_create_registers_per_id_adapter_and_runs_setup(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import get_adapter, set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)

    setup_calls: list[tuple[str, str]] = []
    monkeypatch.setattr(
        host_lifecycle,
        "setup_after_create",
        lambda sid, ws: setup_calls.append((sid, ws)),
    )

    info = provider_control.create_sandbox(name="demo")

    provider.create.assert_called_once()
    assert info["id"] == "sb-1"
    assert get_adapter("sb-1") is provider
    assert setup_calls == [("sb-1", "/workspace/demo")]


def test_start_runs_setup_after_start(monkeypatch: pytest.MonkeyPatch) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import register_adapter

    provider = _stub_provider()
    register_adapter("sb-1", provider)

    setup_calls: list[tuple[str, str]] = []
    monkeypatch.setattr(
        host_lifecycle,
        "setup_after_start",
        lambda sid, ws: setup_calls.append((sid, ws)),
    )

    info = provider_control.start_sandbox("sb-1")

    provider.start.assert_called_once_with("sb-1")
    assert info["state"] == "started"
    assert setup_calls == [("sb-1", "/workspace/demo")]


def test_delete_disposes_adapter_and_plugin_host_caches(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import get_adapter, register_adapter

    provider = _stub_provider()
    register_adapter("sb-1", provider)
    forgotten: list[tuple[str, str]] = []
    monkeypatch.setattr(
        host_lifecycle.plugin_host_dispatch,
        "forget",
        lambda sandbox_id: forgotten.append(("host_dispatch", sandbox_id)),
    )
    monkeypatch.setattr(
        host_lifecycle.plugin_install,
        "forget",
        lambda sandbox_id: forgotten.append(("install", sandbox_id)),
    )

    provider_control.delete_sandbox("sb-1")

    provider.delete.assert_called_once_with("sb-1")
    assert forgotten == [("host_dispatch", "sb-1"), ("install", "sb-1")]
    with pytest.raises(KeyError):
        get_adapter("sb-1")


def test_read_helpers_route_through_registry(monkeypatch: pytest.MonkeyPatch) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.provider.registry import register_adapter, set_default_provider

    default = _stub_provider()
    set_default_provider(default)
    monkeypatch.setattr(
        provider_control,
        "configured_sandbox_defaults",
        lambda: (None, None),
    )

    assert provider_control.list_sandboxes() == [{"id": "sb-1"}]
    assert provider_control.get_health() == {
        "available": True,
        "default_snapshot": None,
        "default_image": None,
    }
    assert provider_control.list_snapshots() == [{"name": "snap"}]

    per_id = _stub_provider()
    register_adapter("sb-1", per_id)
    assert provider_control.get_sandbox("sb-1")["id"] == "sb-1"
    per_id.get.assert_called_once_with("sb-1")
    assert provider_control.get_signed_preview_url("sb-1", 3000) == {
        "url": "https://"
    }
    assert provider_control.get_build_logs_url("sb-1") == "https://logs"


def test_instance_scoped_helpers_fall_back_to_default_provider() -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.provider.registry import get_adapter, set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)

    assert provider_control.get_sandbox("sb-existing")["id"] == "sb-1"
    assert get_adapter("sb-existing") is provider
    provider.get.assert_called_once_with("sb-existing")


def test_set_sandbox_labels_routes_through_provider() -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.provider.registry import register_adapter

    provider = _stub_provider()
    register_adapter("sb-1", provider)

    result = provider_control.set_sandbox_labels("sb-1", {"project_dir": "/testbed"})

    assert result["labels"] == {"project_dir": "/testbed"}
    provider.set_labels.assert_called_once_with("sb-1", {"project_dir": "/testbed"})


def test_create_sandbox_invokes_ensure_git_via_setup_hook(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression: setup_after_create must call ensure_git, not just bootstrap.

    The setup hook must run four steps: start runtime upload,
    ensure_git, finish upload, run bootstrap. Skipping ensure_git breaks
    downstream code that assumes git is installed (sweevo, any consumer
    running ``git ...`` on a minimal-image sandbox).
    """
    import sandbox.api.provider_control as provider_control
    from sandbox.host import bootstrap as bootstrap_mod
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)

    calls: list[str] = []
    monkeypatch.setattr(
        bootstrap_mod, "start_runtime_bundle_upload",
        lambda sid, ws: calls.append(f"start_upload({sid},{ws})") or None,
    )
    monkeypatch.setattr(
        bootstrap_mod, "ensure_git",
        lambda sid: calls.append(f"ensure_git({sid})"),
    )
    monkeypatch.setattr(
        bootstrap_mod, "finish_runtime_bundle_upload",
        lambda fut, sid: calls.append(f"finish_upload({sid})"),
    )
    monkeypatch.setattr(
        bootstrap_mod, "run_runtime_bootstrap",
        lambda sid, ws: calls.append(f"run_bootstrap({sid},{ws})"),
    )
    monkeypatch.setattr(
        bootstrap_mod,
        "ensure_workspace_base",
        lambda sid, ws: calls.append(f"ensure_workspace({sid},{ws})"),
    )

    provider_control.create_sandbox(name="demo")

    assert calls == [
        "start_upload(sb-1,/workspace/demo)",
        "ensure_git(sb-1)",
        "finish_upload(sb-1)",
        "run_bootstrap(sb-1,/workspace/demo)",
        "ensure_workspace(sb-1,/workspace/demo)",
    ]


def test_start_sandbox_invokes_ensure_git_via_setup_hook(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Same regression guard for the start path."""
    import sandbox.api.provider_control as provider_control
    from sandbox.host import bootstrap as bootstrap_mod
    from sandbox.provider.registry import register_adapter

    provider = _stub_provider()
    register_adapter("sb-1", provider)

    calls: list[str] = []
    monkeypatch.setattr(
        bootstrap_mod, "start_runtime_bundle_upload",
        lambda sid, ws: calls.append(f"start_upload({sid},{ws})") or None,
    )
    monkeypatch.setattr(
        bootstrap_mod, "ensure_git",
        lambda sid: calls.append(f"ensure_git({sid})"),
    )
    monkeypatch.setattr(
        bootstrap_mod, "finish_runtime_bundle_upload",
        lambda fut, sid: calls.append(f"finish_upload({sid})"),
    )
    monkeypatch.setattr(
        bootstrap_mod, "run_runtime_bootstrap",
        lambda sid, ws: calls.append(f"run_bootstrap({sid},{ws})"),
    )
    monkeypatch.setattr(
        bootstrap_mod,
        "ensure_workspace_base",
        lambda sid, ws: calls.append(f"ensure_workspace({sid},{ws})"),
    )

    provider_control.start_sandbox("sb-1")

    assert calls == [
        "start_upload(sb-1,/workspace/demo)",
        "ensure_git(sb-1)",
        "finish_upload(sb-1)",
        "run_bootstrap(sb-1,/workspace/demo)",
        "ensure_workspace(sb-1,/workspace/demo)",
    ]


def test_create_sandbox_uses_configured_default_image(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        provider_control,
        "configured_sandbox_defaults",
        lambda: (None, "ghcr.io/example/default:latest"),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    provider_control.create_sandbox(name="demo")

    provider.create.assert_called_once()
    assert provider.create.call_args.kwargs["snapshot"] is None
    assert (
        provider.create.call_args.kwargs["image"]
        == "ghcr.io/example/default:latest"
    )


def test_create_sandbox_explicit_image_overrides_configured_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        provider_control,
        "configured_sandbox_defaults",
        lambda: (None, "ghcr.io/example/default:latest"),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    provider_control.create_sandbox(name="demo", image="ghcr.io/example/custom:latest")

    provider.create.assert_called_once()
    assert provider.create.call_args.kwargs["image"] == "ghcr.io/example/custom:latest"


def test_create_sandbox_snapshot_skips_configured_default_image(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        provider_control,
        "configured_sandbox_defaults",
        lambda: (None, "ghcr.io/example/default:latest"),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    provider_control.create_sandbox(name="demo", snapshot="snap-1")

    provider.create.assert_called_once()
    assert provider.create.call_args.kwargs["snapshot"] == "snap-1"
    assert provider.create.call_args.kwargs["image"] is None


def test_create_sandbox_uses_configured_default_snapshot(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        provider_control,
        "configured_sandbox_defaults",
        lambda: ("sweevo-psf-requests-3738", None),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    provider_control.create_sandbox(name="demo")

    provider.create.assert_called_once()
    assert (
        provider.create.call_args.kwargs["snapshot"]
        == "sweevo-psf-requests-3738"
    )
    assert provider.create.call_args.kwargs["image"] is None


def test_create_sandbox_explicit_image_overrides_configured_default_snapshot(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        provider_control,
        "configured_sandbox_defaults",
        lambda: ("sweevo-psf-requests-3738", None),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    provider_control.create_sandbox(name="demo", image="ghcr.io/example/custom:latest")

    provider.create.assert_called_once()
    assert provider.create.call_args.kwargs["snapshot"] is None
    assert provider.create.call_args.kwargs["image"] == "ghcr.io/example/custom:latest"


def test_configured_default_snapshot_takes_precedence_over_default_image(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from config import CentralConfig, DaytonaConfig, SandboxConfig, override_central_config
    import sandbox.api.provider_control as provider_control
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    config = CentralConfig(
        sandbox=SandboxConfig(
            default_provider="daytona",
            daytona=DaytonaConfig(
                default_snapshot="sweevo-psf-requests-3738",
                default_image="ghcr.io/example/default:latest",
            ),
        )
    )
    with override_central_config(config):
        provider_control.create_sandbox(name="demo")

    provider.create.assert_called_once()
    assert (
        provider.create.call_args.kwargs["snapshot"]
        == "sweevo-psf-requests-3738"
    )
    # WR-06: when both snapshot and image are configured, return both —
    # the caller decides precedence. Pre-fix dropped image whenever
    # snapshot was set, surprising get_health's fallback logic.
    assert (
        provider.create.call_args.kwargs["image"]
        == "ghcr.io/example/default:latest"
    )


def test_health_reports_configured_default_image(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    provider.get_health.return_value = {"available": True, "default_image": None}
    set_default_provider(provider)
    monkeypatch.setattr(
        provider_control,
        "configured_sandbox_defaults",
        lambda: (None, "ghcr.io/example/default:latest"),
    )

    assert provider_control.get_health() == {
        "available": True,
        "default_snapshot": None,
        "default_image": "ghcr.io/example/default:latest",
    }


def test_health_reports_configured_default_snapshot(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import sandbox.api.provider_control as provider_control
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    provider.get_health.return_value = {
        "available": True,
        "default_snapshot": None,
        "default_image": None,
    }
    set_default_provider(provider)
    monkeypatch.setattr(
        provider_control,
        "configured_sandbox_defaults",
        lambda: ("sweevo-psf-requests-3738", None),
    )

    assert provider_control.get_health() == {
        "available": True,
        "default_snapshot": "sweevo-psf-requests-3738",
        "default_image": None,
    }
