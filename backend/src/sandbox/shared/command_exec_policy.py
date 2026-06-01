"""Policy values for guarded command execution."""

from __future__ import annotations

import os
from collections.abc import Mapping
from dataclasses import dataclass, field


@dataclass(frozen=True)
class CommandExecPolicy:
    """Tenant/test-injectable command execution policy."""

    host_env_keys: frozenset[str] | None = None
    restricted_env_keys: frozenset[str] = field(
        default_factory=lambda: frozenset(
            {
                "LD_PRELOAD",
                "LD_LIBRARY_PATH",
                "LD_AUDIT",
                "DYLD_INSERT_LIBRARIES",
                "DYLD_LIBRARY_PATH",
                "PATH",
                "PYTHONPATH",
                "BASH_ENV",
                "ENV",
            }
        )
    )
    forbidden_overlay_path_chars: tuple[str, ...] = (
        ",",
        ":",
        "\\",
        "\n",
        "\r",
        "\t",
        "\0",
    )
    command_env_defaults: Mapping[str, str] = field(
        default_factory=lambda: {"GIT_OPTIONAL_LOCKS": "0"}
    )

    def command_environment(self, extra: Mapping[str, str]) -> dict[str, str]:
        host_env = (
            dict(os.environ)
            if self.host_env_keys is None
            else {key: os.environ[key] for key in self.host_env_keys if key in os.environ}
        )
        safe_extra = {k: v for k, v in extra.items() if k not in self.restricted_env_keys}
        env = {
            **host_env,
            **safe_extra,
            **{str(k): str(v) for k, v in self.command_env_defaults.items()},
        }
        base_path = env.get(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        env["PATH"] = (
            "/opt/miniconda3/envs/testbed/bin:/opt/miniconda3/bin:"
            f"{base_path}"
        )
        env.pop("BASH_ENV", None)
        env.pop("ENV", None)
        return env

    def validate_overlay_path_text(self, text: str) -> None:
        for bad in self.forbidden_overlay_path_chars:
            if bad in text:
                label = repr(bad)
                raise ValueError(f"overlay mount path cannot contain {label}: {text!r}")

    def to_payload(self) -> dict[str, object]:
        return {
            "host_env_keys": (
                sorted(self.host_env_keys) if self.host_env_keys is not None else None
            ),
            "restricted_env_keys": sorted(self.restricted_env_keys),
            "forbidden_overlay_path_chars": list(self.forbidden_overlay_path_chars),
            "command_env_defaults": dict(self.command_env_defaults),
        }

    @classmethod
    def from_payload(cls, payload: Mapping[str, object]) -> CommandExecPolicy:
        defaults = DEFAULT_COMMAND_EXEC_POLICY
        env_defaults_raw = payload.get("command_env_defaults")
        env_defaults = (
            {str(k): str(v) for k, v in env_defaults_raw.items()}
            if isinstance(env_defaults_raw, Mapping)
            else dict(defaults.command_env_defaults)
        )
        forbidden_raw = payload.get("forbidden_overlay_path_chars")
        forbidden = (
            tuple(str(item) for item in forbidden_raw)
            if isinstance(forbidden_raw, list)
            else defaults.forbidden_overlay_path_chars
        )
        host_env_keys_raw = payload.get("host_env_keys")
        host_env_keys = (
            frozenset(str(item) for item in host_env_keys_raw)
            if isinstance(host_env_keys_raw, list)
            else None
        )
        return cls(
            host_env_keys=host_env_keys,
            restricted_env_keys=_string_set(
                payload.get("restricted_env_keys"),
                default=defaults.restricted_env_keys,
            ),
            forbidden_overlay_path_chars=forbidden,
            command_env_defaults=env_defaults,
        )


def _string_set(raw: object, *, default: frozenset[str]) -> frozenset[str]:
    if not isinstance(raw, list):
        return default
    return frozenset(str(item) for item in raw)


DEFAULT_COMMAND_EXEC_POLICY = CommandExecPolicy()

__all__ = [
    "CommandExecPolicy",
    "DEFAULT_COMMAND_EXEC_POLICY",
]
