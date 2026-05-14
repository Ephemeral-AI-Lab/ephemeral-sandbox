"""Unit tests for public sandbox lifecycle, discovery, and URL modules."""

from __future__ import annotations

import json
import threading
from pathlib import Path
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
    from sandbox.api import _control as sb_lifecycle
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

    info = sb_lifecycle.create_sandbox(name="demo")

    provider.create.assert_called_once()
    assert info["id"] == "sb-1"
    assert get_adapter("sb-1") is provider
    assert setup_calls == [("sb-1", "/workspace/demo")]


def test_start_runs_setup_after_start(monkeypatch: pytest.MonkeyPatch) -> None:
    from sandbox.api import _control as sb_lifecycle
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

    info = sb_lifecycle.start_sandbox("sb-1")

    provider.start.assert_called_once_with("sb-1")
    assert info["state"] == "started"
    assert setup_calls == [("sb-1", "/workspace/demo")]


def test_delete_disposes_adapter_and_plugin_host_caches(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from sandbox.api import _control as sb_lifecycle
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import get_adapter, register_adapter

    provider = _stub_provider()
    register_adapter("sb-1", provider)
    forgotten: list[tuple[str, str]] = []
    monkeypatch.setattr(
        host_lifecycle.plugin_session,
        "forget",
        lambda sandbox_id: forgotten.append(("session", sandbox_id)),
    )
    monkeypatch.setattr(
        host_lifecycle.plugin_install,
        "forget",
        lambda sandbox_id: forgotten.append(("install", sandbox_id)),
    )

    sb_lifecycle.delete_sandbox("sb-1")

    provider.delete.assert_called_once_with("sb-1")
    assert forgotten == [("session", "sb-1"), ("install", "sb-1")]
    with pytest.raises(KeyError):
        get_adapter("sb-1")


def test_read_helpers_route_through_registry(monkeypatch: pytest.MonkeyPatch) -> None:
    from sandbox.api import _control as sb_discovery
    from sandbox.api import _control as sb_preview_urls
    from sandbox.provider.registry import register_adapter, set_default_provider

    default = _stub_provider()
    set_default_provider(default)
    monkeypatch.setattr(
        sb_discovery,
        "configured_sandbox_defaults",
        lambda: (None, None),
    )

    assert sb_discovery.list_sandboxes() == [{"id": "sb-1"}]
    assert sb_discovery.get_health() == {
        "available": True,
        "default_snapshot": None,
        "default_image": None,
    }
    assert sb_discovery.list_snapshots() == [{"name": "snap"}]

    per_id = _stub_provider()
    register_adapter("sb-1", per_id)
    assert sb_discovery.get_sandbox("sb-1")["id"] == "sb-1"
    per_id.get.assert_called_once_with("sb-1")
    assert sb_preview_urls.get_signed_preview_url("sb-1", 3000) == {
        "url": "https://"
    }
    assert sb_preview_urls.get_build_logs_url("sb-1") == "https://logs"


def test_instance_scoped_helpers_fall_back_to_default_provider() -> None:
    from sandbox.api import _control as sb_discovery
    from sandbox.provider.registry import get_adapter, set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)

    assert sb_discovery.get_sandbox("sb-existing")["id"] == "sb-1"
    assert get_adapter("sb-existing") is provider
    provider.get.assert_called_once_with("sb-existing")


def test_set_sandbox_labels_routes_through_provider() -> None:
    from sandbox.api import _control as sb_lifecycle
    from sandbox.provider.registry import register_adapter

    provider = _stub_provider()
    register_adapter("sb-1", provider)

    result = sb_lifecycle.set_sandbox_labels("sb-1", {"project_dir": "/testbed"})

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
    from sandbox.api import _control as sb_lifecycle
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

    sb_lifecycle.create_sandbox(name="demo")

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
    from sandbox.api import _control as sb_lifecycle
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

    sb_lifecycle.start_sandbox("sb-1")

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
    from sandbox.api import _control as sb_lifecycle
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        sb_lifecycle,
        "configured_sandbox_defaults",
        lambda: (None, "ghcr.io/example/default:latest"),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    sb_lifecycle.create_sandbox(name="demo")

    provider.create.assert_called_once()
    assert provider.create.call_args.kwargs["snapshot"] is None
    assert (
        provider.create.call_args.kwargs["image"]
        == "ghcr.io/example/default:latest"
    )


def test_create_sandbox_explicit_image_overrides_configured_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from sandbox.api import _control as sb_lifecycle
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        sb_lifecycle,
        "configured_sandbox_defaults",
        lambda: (None, "ghcr.io/example/default:latest"),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    sb_lifecycle.create_sandbox(name="demo", image="ghcr.io/example/custom:latest")

    provider.create.assert_called_once()
    assert provider.create.call_args.kwargs["image"] == "ghcr.io/example/custom:latest"


def test_create_sandbox_snapshot_skips_configured_default_image(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from sandbox.api import _control as sb_lifecycle
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        sb_lifecycle,
        "configured_sandbox_defaults",
        lambda: (None, "ghcr.io/example/default:latest"),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    sb_lifecycle.create_sandbox(name="demo", snapshot="snap-1")

    provider.create.assert_called_once()
    assert provider.create.call_args.kwargs["snapshot"] == "snap-1"
    assert provider.create.call_args.kwargs["image"] is None


def test_create_sandbox_uses_configured_default_snapshot(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from sandbox.api import _control as sb_lifecycle
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        sb_lifecycle,
        "configured_sandbox_defaults",
        lambda: ("sweevo-psf-requests-3738", None),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    sb_lifecycle.create_sandbox(name="demo")

    provider.create.assert_called_once()
    assert (
        provider.create.call_args.kwargs["snapshot"]
        == "sweevo-psf-requests-3738"
    )
    assert provider.create.call_args.kwargs["image"] is None


def test_create_sandbox_explicit_image_overrides_configured_default_snapshot(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from sandbox.api import _control as sb_lifecycle
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(
        sb_lifecycle,
        "configured_sandbox_defaults",
        lambda: ("sweevo-psf-requests-3738", None),
    )
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    sb_lifecycle.create_sandbox(name="demo", image="ghcr.io/example/custom:latest")

    provider.create.assert_called_once()
    assert provider.create.call_args.kwargs["snapshot"] is None
    assert provider.create.call_args.kwargs["image"] == "ghcr.io/example/custom:latest"


def test_configured_default_snapshot_takes_precedence_over_default_image(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from sandbox.api import _control as sb_lifecycle
    from sandbox.host import lifecycle as host_lifecycle
    from sandbox.provider.registry import set_default_provider

    settings_path = tmp_path / "settings.json"
    settings_path.write_text(
        json.dumps(
            {
                "sandbox": {
                    "default_snapshot": "sweevo-psf-requests-3738",
                    "default_image": "ghcr.io/example/default:latest",
                },
            }
        )
    )
    monkeypatch.setattr("config.paths.get_config_file_path", lambda: settings_path)
    monkeypatch.setattr("config.settings._DOTENV_PATH", tmp_path / ".env")
    monkeypatch.delenv("EPHEMERALOS_SANDBOX_DEFAULT_IMAGE", raising=False)
    monkeypatch.delenv("EPHEMERALOS_SANDBOX_DEFAULT_SNAPSHOT", raising=False)

    provider = _stub_provider()
    set_default_provider(provider)
    monkeypatch.setattr(host_lifecycle, "setup_after_create", lambda sid, ws: None)

    sb_lifecycle.create_sandbox(name="demo")

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
    from sandbox.api import _control as sb_discovery
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    provider.get_health.return_value = {"available": True, "default_image": None}
    set_default_provider(provider)
    monkeypatch.setattr(
        sb_discovery,
        "configured_sandbox_defaults",
        lambda: (None, "ghcr.io/example/default:latest"),
    )

    assert sb_discovery.get_health() == {
        "available": True,
        "default_snapshot": None,
        "default_image": "ghcr.io/example/default:latest",
    }


def test_health_reports_configured_default_snapshot(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from sandbox.api import _control as sb_discovery
    from sandbox.provider.registry import set_default_provider

    provider = _stub_provider()
    provider.get_health.return_value = {
        "available": True,
        "default_snapshot": None,
        "default_image": None,
    }
    set_default_provider(provider)
    monkeypatch.setattr(
        sb_discovery,
        "configured_sandbox_defaults",
        lambda: ("sweevo-psf-requests-3738", None),
    )

    assert sb_discovery.get_health() == {
        "available": True,
        "default_snapshot": "sweevo-psf-requests-3738",
        "default_image": None,
    }
