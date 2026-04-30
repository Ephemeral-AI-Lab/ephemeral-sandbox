"""Tests for sandbox.credentials."""

from __future__ import annotations



class TestLoadCredentials:
    def test_loads_from_env(self, monkeypatch):
        monkeypatch.setenv("DAYTONA_API_KEY", "key-from-env")
        monkeypatch.setenv("DAYTONA_API_URL", "https://url-from-env")
        monkeypatch.setenv("DAYTONA_TARGET", "target-from-env")

        from sandbox.client.credentials import load_credentials

        key, url, target = load_credentials()
        assert key == "key-from-env"
        assert url == "https://url-from-env"
        assert target == "target-from-env"

    def test_settings_fallback(self, monkeypatch):
        monkeypatch.delenv("DAYTONA_API_KEY", raising=False)
        monkeypatch.delenv("DAYTONA_API_URL", raising=False)
        monkeypatch.delenv("DAYTONA_TARGET", raising=False)

        import sys
        import types

        class FakeSettingsObj:
            def __init__(self):
                self.daytona_api_key = "  key-from-settings "
                self.daytona_api_url = "  https://url-from-settings "
                self.daytona_target = "  target-from-settings "

        def fake_load_settings():
            return FakeSettingsObj()

        fake_settings = types.ModuleType("config.settings")
        fake_settings.load_settings = fake_load_settings
        monkeypatch.setitem(sys.modules, "config", fake_settings)
        monkeypatch.setitem(sys.modules, "config.settings", fake_settings)

        from sandbox.client.credentials import load_credentials

        key, url, target = load_credentials()
        assert key == "key-from-settings"
        assert url == "https://url-from-settings"
        assert target == "target-from-settings"

    def test_env_takes_precedence(self, monkeypatch):
        monkeypatch.setenv("DAYTONA_API_KEY", "env-key")
        monkeypatch.setenv("DAYTONA_API_URL", "https://env-url")

        import sys
        import types

        fake_settings = types.ModuleType("config.settings")
        fake_settings.FakeSettings = type(
            "FakeSettings",
            (),
            {
                "daytona_api_key": "settings-key",
                "daytona_api_url": "https://settings-url",
                "daytona_target": "",
            },
        )()
        monkeypatch.setitem(sys.modules, "config", fake_settings)
        monkeypatch.setitem(sys.modules, "config.settings", fake_settings)

        from sandbox.client.credentials import load_credentials

        key, url, target = load_credentials()
        assert key == "env-key"
        assert url == "https://env-url"

    def test_returns_empty_when_unconfigured(self, monkeypatch):
        monkeypatch.delenv("DAYTONA_API_KEY", raising=False)
        monkeypatch.delenv("DAYTONA_API_URL", raising=False)
        monkeypatch.delenv("DAYTONA_TARGET", raising=False)

        import sys
        import types

        fake_settings = types.ModuleType("config.settings")
        fake_settings.FakeSettings = type(
            "FakeSettings",
            (),
            {
                "daytona_api_key": "",
                "daytona_api_url": "",
                "daytona_target": "",
            },
        )()
        monkeypatch.setitem(sys.modules, "config", fake_settings)
        monkeypatch.setitem(sys.modules, "config.settings", fake_settings)

        from sandbox.client.credentials import load_credentials

        key, url, target = load_credentials()
        assert key == ""
        assert url == ""
        assert target == ""
