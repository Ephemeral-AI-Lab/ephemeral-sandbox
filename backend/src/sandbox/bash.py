"""Bash wrapping helpers shared by host adapters and bundled runtime code."""

from __future__ import annotations

import re
import shlex

EXIT_MARKER = "__CODEX_EXIT_CODE__="

_USER_LOCAL_BIN_EXPORT = 'export PATH="$HOME/.local/bin:$PATH"'
_PROJECT_VENV_BIN_EXPORT = 'if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi'
_PYTHON3_SHIM = (
    'if ! command -v python >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1; '
    'then python() { command python3 "$@"; }; fi'
)
_TRAILING_TERM_NOISE_RE = re.compile(
    r"(?:\x1b\[[0-9;]*[A-Za-z]|TERM environment variable not set\.)+\s*$"
)


def wrap_bash_command(command: str, *, cwd: str | None = None) -> str:
    cd_command = f"cd {shlex.quote(cwd)}\n" if cwd else ""
    script = (
        f"{_USER_LOCAL_BIN_EXPORT}\n"
        f"{cd_command}"
        f"{_PROJECT_VENV_BIN_EXPORT}\n"
        f"{_PYTHON3_SHIM}\n"
        f"{command}\n"
        "__codex_exit_code=$?\n"
        f'printf "\\n{EXIT_MARKER}%s\\n" "$__codex_exit_code"\n'
        'exit "$__codex_exit_code"'
    )
    return f"env -u LC_ALL bash -o pipefail -lc {shlex.quote(script)}"


def extract_exit_code(
    output: str,
    *,
    fallback_exit_code: int | str | None,
) -> tuple[str, int]:
    sanitized = _TRAILING_TERM_NOISE_RE.sub("", output or "").rstrip()
    matches = list(
        re.finditer(rf"\n?{re.escape(EXIT_MARKER)}(-?\d+)", sanitized, flags=re.S)
    )
    if matches:
        marker = matches[-1]
        resolved = int(marker.group(1))
        cleaned = sanitized[: marker.start()]
        if cleaned.endswith("\n"):
            cleaned = cleaned[:-1]
        return cleaned, resolved
    if fallback_exit_code is None:
        return sanitized, 0
    if isinstance(fallback_exit_code, int):
        return sanitized, fallback_exit_code
    stripped = fallback_exit_code.strip()
    if stripped.lstrip("-").isdigit():
        return sanitized, int(stripped)
    return sanitized, 0


__all__ = ["EXIT_MARKER", "extract_exit_code", "wrap_bash_command"]
