"""CodeAct tool - shell or Python execution in the Daytona sandbox."""

from __future__ import annotations

import base64
import json
import re
import shlex
import uuid
from typing import Any, Literal

from pydantic import BaseModel, Field
from pydantic.json_schema import GenerateJsonSchema

from code_intelligence.tuning import CODE_INTELLIGENCE_TUNING
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import (
    ci_required_result,
    exec_ci_process_operation,
    get_ci_service,
)
from tools.core.decorator import tool
from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _format_shell_stdout,
    _get_cwd,
    _read_text_file_via_exec,
    _recover_sandbox,
    _require_sandbox,
    _team_repo_write_error,
    _team_repo_write_warning,
    _supports_exec_transport,
    _upload_file_compat,
    _write_text_file_via_exec,
    _wrap_bash_command,
    is_coordinated_team_agent,
)
from tools.daytona_toolkit._shell_policy import shell_policy_source

_DESTRUCTIVE_GIT_PATTERN = re.compile(
    r"git\s+(stash|reset\s+--hard|checkout\s+--\s|checkout\s+\.\s*$|clean\s+-[fd])",
    flags=re.IGNORECASE,
)
_CODEACT_FILE_EDIT_POLICY_MESSAGE = (
    "BLOCKED: daytona_codeact is for runtime commands, tests, and inspection in "
    "coordinated team lanes. File edits must use daytona_edit_file, "
    "daytona_write_file, daytona_rename_symbol, daytona_delete_file, or "
    "daytona_move_file so write-scope, OCC, and invalid-edit guardrails run "
    "before mutation. Use daytona_delete_file for removals and "
    "daytona_move_file for path moves. Do not retry cleanup with rm, mv, "
    "unlink, os.remove, Path.unlink, shutil.rmtree, shutil.move, git rm, or "
    "git mv inside CodeAct."
)
_SHELL_FILE_EDIT_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = (
    (
        re.compile(
            r"(?:^|[;&|]\s*)(?:sudo\s+)?(?:g?sed|sed)\b(?:(?![;&|]).)*\s-[A-Za-z]*i(?:\b|[=.])",
            flags=re.IGNORECASE | re.DOTALL,
        ),
        "in-place sed",
    ),
    (
        re.compile(
            r"(?:^|[;&|]\s*)perl\b(?:(?![;&|]).)*\s-\S*i\S*",
            flags=re.IGNORECASE | re.DOTALL,
        ),
        "in-place perl",
    ),
    (
        re.compile(
            r"(?:^|[;&|]\s*)tee\b(?:\s+-[A-Za-z]+)*\s+(?!/dev/null(?:\s|$))\S+",
            flags=re.IGNORECASE,
        ),
        "tee file write",
    ),
    (
        re.compile(
            r"(?:^|[;&|]\s*)(?:touch|truncate|cp|mv|install|rm|rmdir)\b"
            r"|(?:^|[;&|]\s*)git\s+(?:rm|mv)\b",
            flags=re.IGNORECASE,
        ),
        "filesystem mutation command",
    ),
    (
        re.compile(
            r"(?:^|[;&|]\s*)python(?:3(?:\.\d+)?)?\b.*"
            r"(?:write_text|write_bytes|"
            r"\bopen\s*\([^)]*,\s*['\"][^'\"]*[wax+]|"
            r"\bshutil\.|\bos\.(?:remove|unlink|rename|replace)|"
            r"\bPath\s*\([^)]*\)\.(?:touch|unlink|rename|replace|mkdir))",
            flags=re.IGNORECASE | re.DOTALL,
        ),
        "inline Python file mutation",
    ),
)
_SHELL_OUTPUT_REDIRECTION_PATTERN = re.compile(
    r"(?<![<>&])(?:\b\d*)?(?:>>?|&>)\s*(?!&\d\b)(?!/dev/null(?:\s|$))\S+",
    flags=re.IGNORECASE,
)
_PYTHON_FILE_EDIT_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"(?<![.\w])write\s*\(", flags=re.IGNORECASE), "CodeAct write() helper"),
    (re.compile(r"\bwrite_text\s*\(", flags=re.IGNORECASE), "Path.write_text"),
    (re.compile(r"\bwrite_bytes\s*\(", flags=re.IGNORECASE), "Path.write_bytes"),
    (
        re.compile(
            r"\bopen\s*\([^)]*,\s*['\"][^'\"]*[wax+]",
            flags=re.IGNORECASE | re.DOTALL,
        ),
        "write-mode open()",
    ),
    (
        re.compile(
            r"\b(?:os|Path\s*\([^)]*\))\.(?:remove|unlink|rename|replace|touch|mkdir|rmdir)\b",
            flags=re.IGNORECASE | re.DOTALL,
        ),
        "Python filesystem mutation",
    ),
    (re.compile(r"\bshutil\.", flags=re.IGNORECASE), "shutil file mutation"),
)
_CODEACT_DEFAULT_TIMEOUT = CODE_INTELLIGENCE_TUNING.codeact_default_timeout
_CODEACT_WRITE_TIMEOUT = CODE_INTELLIGENCE_TUNING.codeact_write_timeout


class DaytonaCodeActInput(BaseModel):
    """Custom CodeAct input schema.

    Keep runtime parsing permissive so existing callers still flow through
    ``_resolve_mode()``, but publish a stricter JSON schema to the model.
    Anthropic-compatible models will otherwise happily emit explicit JSON
    ``null`` for optional string params and spin on empty CodeAct calls.
    """

    mode: Literal["python", "shell"] | None = Field(
        default=None,
        description=(
            "Optional explicit mode. Omit unless you need to force shell or "
            "python execution."
        ),
    )
    code: str | None = Field(
        default=None,
        description=(
            "Python code to execute. Use for multi-step helper flows; do not "
            "set alongside `command`."
        ),
    )
    command: str | None = Field(
        default=None,
        description=(
            "Shell command to execute directly. Preferred for tests, builds, "
            "and verification; do not set alongside `code`."
        ),
    )
    timeout: int = Field(
        default=_CODEACT_DEFAULT_TIMEOUT,
        description="Timeout in seconds for shell mode execution.",
    )

    @classmethod
    def model_json_schema(
        cls,
        by_alias: bool = True,
        ref_template: str = "#/$defs/{model}",
        schema_generator: type[GenerateJsonSchema] = GenerateJsonSchema,
        mode: str = "validation",
    ) -> dict[str, Any]:
        schema = super().model_json_schema(
            by_alias=by_alias,
            ref_template=ref_template,
            schema_generator=schema_generator,
            mode=mode,
        )
        props = schema.get("properties", {})

        def _strip_null_variant(name: str, expected_type: str) -> None:
            prop = props.get(name)
            if not isinstance(prop, dict):
                return
            cleaned: dict[str, Any] | None = None
            for variant in prop.get("anyOf", []):
                if isinstance(variant, dict) and variant.get("type") == expected_type:
                    cleaned = dict(variant)
                    break
            if cleaned is None:
                return
            if "title" in prop:
                cleaned["title"] = prop["title"]
            if "description" in prop:
                cleaned["description"] = prop["description"]
            cleaned.pop("default", None)
            if expected_type == "string":
                cleaned["minLength"] = max(int(cleaned.get("minLength", 1) or 1), 1)
            props[name] = cleaned

        _strip_null_variant("mode", "string")
        _strip_null_variant("code", "string")
        _strip_null_variant("command", "string")

        schema["oneOf"] = [
            {"required": ["command"]},
            {"required": ["code"]},
        ]
        return schema


class DaytonaCodeActShellOutput(BaseModel):
    command: str = Field(..., description="Shell command that was run.")
    exit_code: int | str = Field(..., description="Command exit code.")
    stdout: str = Field(..., description="Captured stdout.")
    stderr: str = Field(..., description="Captured stderr.")


class DaytonaCodeActOutput(BaseModel):
    cwd: str = Field(..., description="Current sandbox working directory.")
    status: str = Field(..., description="Execution status: ok or error.")
    files_written: int = Field(
        ...,
        description="Number of helper or audited process file writes observed.",
    )
    shells_run: int = Field(..., description="Number of shell commands executed.")
    shell_summaries: list[str] = Field(
        default_factory=list,
        description="Compact summaries of the first shell commands.",
    )
    shell_outputs: list[DaytonaCodeActShellOutput] = Field(
        default_factory=list,
        description="Captured output for the first shell commands.",
    )
    script_stdout: str = Field(..., description="Python wrapper stdout before the manifest line.")
    warnings: list[str] = Field(default_factory=list, description="Non-fatal warnings.")
    error: str = Field(default="", description="Error detail when status is error.")


def _destructive_git_command_error(command: str) -> str | None:
    if _DESTRUCTIVE_GIT_PATTERN.search(command or ""):
        return (
            "BLOCKED: destructive git commands (stash, reset --hard, checkout --, clean) "
            "are forbidden. They destroy other agents' work and bypass process audit. "
            "Use targeted edit tools instead."
        )
    return None


def _format_codeact_error(
    *,
    stdout: str,
    manifest_error: str = "",
) -> str:
    detail = manifest_error.strip() or stdout[:4000]
    lines = ["CodeAct execution error:"]
    if detail:
        lines.append(detail)
    if "blocked in codeact" in detail or "subprocess" in detail or "os.system" in detail:
        lines.append(
            "Use `daytona_codeact(command=\"...\")` or `shell(\"...\")` inside Python mode; "
            "do not import `subprocess` or call `os.system()`."
        )
    if "daytona_codeact is for runtime commands" in detail:
        lines.append(
            "Use `daytona_edit_file`, `daytona_write_file`, "
            "`daytona_rename_symbol`, `daytona_delete_file`, or "
            "`daytona_move_file` for file changes."
        )
    return "\n".join(lines)


def _has_team_task_context(context: ToolExecutionContext) -> bool:
    return bool(
        context.metadata.get("task_center")
        or context.metadata.get("team_run_id")
        or context.metadata.get("work_item_id")
        or context.metadata.get("benchmark_test_ids")
        or context.metadata.get("benchmark_test_files")
    )


def _enforce_codeact_file_edit_policy(context: ToolExecutionContext) -> bool:
    return is_coordinated_team_agent(context) and _has_team_task_context(context)


def _file_edit_policy_error(kind: str) -> str:
    return f"{_CODEACT_FILE_EDIT_POLICY_MESSAGE} Detected {kind}."


def _mask_shell_quoted_text(command: str) -> str:
    """Mask shell-quoted text while keeping quote delimiters and rough token shape."""
    out: list[str] = []
    quote: str | None = None
    escaped = False
    for char in command:
        if escaped:
            out.append("x" if quote else char)
            escaped = False
            continue
        if char == "\\":
            out.append("x" if quote else char)
            escaped = True
            continue
        if quote:
            if char == quote:
                quote = None
                out.append(char)
            else:
                out.append("x" if not char.isspace() else char)
            continue
        if char in {"'", '"'}:
            quote = char
        out.append(char)
    return "".join(out)


def _shell_file_edit_policy_error(command: str) -> str | None:
    if _SHELL_OUTPUT_REDIRECTION_PATTERN.search(_mask_shell_quoted_text(command or "")):
        return _file_edit_policy_error("shell output redirection")
    for pattern, kind in _SHELL_FILE_EDIT_PATTERNS:
        if pattern.search(command or ""):
            return _file_edit_policy_error(kind)
    return None


def _python_file_edit_policy_error(code: str) -> str | None:
    for pattern, kind in _PYTHON_FILE_EDIT_PATTERNS:
        if pattern.search(code or ""):
            return _file_edit_policy_error(kind)
    return None


def _python_literal_or_none(value: str | None) -> str:
    if not value or str(value).strip().lower() == "none":
        return "None"
    return json.dumps(value)


_WRAPPER_TEMPLATE = r'''
import base64, hashlib, importlib, io, json, os, pathlib, re, shlex, subprocess, traceback

_RUN_ID = "{run_id}"
_MANIFEST = {{"reads": [], "writes": [], "shells": [], "status": "ok", "error": ""}}
_CODEACT_CWD = {codeact_cwd}
_CODEACT_REPO_ROOT = {codeact_repo_root}
_ENFORCE_TEAM_SHELL_POLICY = {enforce_team_shell_policy}
_DISABLE_CODEACT_FILE_EDITS = {disable_codeact_file_edits}
_CODEACT_FILE_EDIT_POLICY_MESSAGE = {codeact_file_edit_policy_message}
_USER_LOCAL_BIN_EXPORT = 'export PATH="$HOME/.local/bin:$PATH"'
_PROJECT_VENV_BIN_EXPORT = 'if [ -d .venv/bin ]; then export PATH="$PWD/.venv/bin:$PATH"; fi'
_PYTHON3_SHIM = 'if command -v python3 >/dev/null 2>&1; then python() {{ command python3 "$@"; }}; fi'
_BLOCKED_MODULES = frozenset({{"subprocess", "shutil"}})
_DESTRUCTIVE_GIT_PATTERN = re.compile(
    r"git\s+(stash|reset\s+--hard|checkout\s+--\s|checkout\s+\.\s*$|clean\s+-[fd])",
    flags=re.IGNORECASE,
)
_DESTRUCTIVE_SHELL_PATTERN = re.compile(
    r"(?:^|[;&|]\s*)(?:"
    r"rm\s+(?:-\S*[rR]\S*\s+|--recursive\s+)(?:/(?:testbed|workspace|home|opt|usr|var|etc|tmp)\b|/\s|/\.\.|\.\.)"
    r"|mv\s+/(?:testbed|workspace|home|opt|usr|var|etc)(?:/[^/\s]*)?(?:\s|$)"
    r"|chmod\s+(?:-\S*R\S*\s+|--recursive\s+)\S*\s+/"
    r"|chown\s+(?:-\S*R\S*\s+|--recursive\s+)\S*\s+/"
    r"|rm\s+-\S*[rR]\S*\s+\.\s*$"
    r"|mkfs\b|dd\s+.*of=/"
    r")",
    flags=re.IGNORECASE,
)
_CODEACT_SHELL_FILE_EDIT_PATTERNS = (
    (
        re.compile(
            r"(?:^|[;&|]\s*)(?:sudo\s+)?(?:g?sed|sed)\b(?:(?![;&|]).)*\s-[A-Za-z]*i(?:\b|[=.])",
            flags=re.IGNORECASE | re.DOTALL,
        ),
        "in-place sed",
    ),
    (
        re.compile(
            r"(?:^|[;&|]\s*)perl\b(?:(?![;&|]).)*\s-\S*i\S*",
            flags=re.IGNORECASE | re.DOTALL,
        ),
        "in-place perl",
    ),
    (
        re.compile(
            r"(?:^|[;&|]\s*)tee\b(?:\s+-[A-Za-z]+)*\s+(?!/dev/null(?:\s|$))\S+",
            flags=re.IGNORECASE,
        ),
        "tee file write",
    ),
    (
        re.compile(
            r"(?:^|[;&|]\s*)(?:touch|truncate|cp|mv|install|rm|rmdir)\b"
            r"|(?:^|[;&|]\s*)git\s+(?:rm|mv)\b",
            flags=re.IGNORECASE,
        ),
        "filesystem mutation command",
    ),
    (
        re.compile(
            r"(?:^|[;&|]\s*)python(?:3(?:\.\d+)?)?\b.*"
            r"(?:write_text|write_bytes|"
            r"\bopen\s*\([^)]*,\s*['\"][^'\"]*[wax+]|"
            r"\bshutil\.|\bos\.(?:remove|unlink|rename|replace)|"
            r"\bPath\s*\([^)]*\)\.(?:touch|unlink|rename|replace|mkdir))",
            flags=re.IGNORECASE | re.DOTALL,
        ),
        "inline Python file mutation",
    ),
)
_CODEACT_SHELL_OUTPUT_REDIRECTION_PATTERN = re.compile(
    r"(?<![<>&])(?:\b\d*)?(?:>>?|&>)\s*(?!&\d\b)(?!/dev/null(?:\s|$))\S+",
    flags=re.IGNORECASE,
)
{shell_policy_source}

def _mask_shell_quoted_text(command):
    out = []
    quote = None
    escaped = False
    for char in command:
        if escaped:
            out.append("x" if quote else char)
            escaped = False
            continue
        if char == "\\":
            out.append("x" if quote else char)
            escaped = True
            continue
        if quote:
            if char == quote:
                quote = None
                out.append(char)
            else:
                out.append("x" if not char.isspace() else char)
            continue
        if char in {{"'", '"'}}:
            quote = char
        out.append(char)
    return "".join(out)

def _codeact_shell_file_edit_error(command):
    if _CODEACT_SHELL_OUTPUT_REDIRECTION_PATTERN.search(_mask_shell_quoted_text(command or "")):
        return "shell output redirection"
    for pattern, kind in _CODEACT_SHELL_FILE_EDIT_PATTERNS:
        if pattern.search(command or ""):
            return kind
    return None

def _normalize_path(path):
    if os.path.isabs(path):
        return path
    return os.path.abspath(path)

def read(path):
    resolved = _normalize_path(path)
    with open(resolved, "r", encoding="utf-8") as f:
        content = f.read()
    h = hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
    _MANIFEST["reads"].append({{"path": resolved, "hash": h}})
    return content

def write(path, content):
    if _DISABLE_CODEACT_FILE_EDITS:
        raise RuntimeError(_CODEACT_FILE_EDIT_POLICY_MESSAGE)
    resolved = _normalize_path(path)
    parent = os.path.dirname(resolved)
    if parent:
        os.makedirs(parent, exist_ok=True)
    with open(resolved, "w", encoding="utf-8") as f:
        f.write(content)
    _MANIFEST["writes"].append({{"path": resolved, "content": content}})
    return resolved

def _block_shell_command(command, message):
    _MANIFEST["shells"].append(
        {{
            "command": command,
            "stdout": "",
            "stderr": message,
            "exit_code": -1,
            "blocked": True,
        }}
    )
    raise RuntimeError(message)

def shell(command, timeout={codeact_default_timeout}):
    if _ENFORCE_TEAM_SHELL_POLICY:
        command, policy_warnings = _normalize_team_shell_command(
            command,
            repo_root=_CODEACT_REPO_ROOT,
        )
        _MANIFEST.setdefault("warnings", []).extend(policy_warnings)
    if _DESTRUCTIVE_GIT_PATTERN.search(command or ""):
        _block_shell_command(
            command,
            "BLOCKED: destructive git commands (stash, reset --hard, checkout --, clean) "
            "are forbidden. They destroy other agents' work and bypass process audit. "
            "Use targeted edit tools instead.",
        )
    if _DESTRUCTIVE_SHELL_PATTERN.search(command or ""):
        _block_shell_command(
            command,
            "BLOCKED: destructive shell command that targets workspace or system "
            "directories is forbidden. Use targeted file operations instead.",
        )
    if _DISABLE_CODEACT_FILE_EDITS:
        edit_kind = _codeact_shell_file_edit_error(command)
        if edit_kind:
            _block_shell_command(
                command,
                f"{{_CODEACT_FILE_EDIT_POLICY_MESSAGE}} Detected {{edit_kind}}.",
            )
    try:
        wrapped = f"{{_USER_LOCAL_BIN_EXPORT}} && {{_PROJECT_VENV_BIN_EXPORT}} && {{_PYTHON3_SHIM}} && {{command}}"
        proc = subprocess.run(
            ["env", "-u", "LC_ALL", "bash", "-o", "pipefail", "-lc", wrapped],
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=_CODEACT_CWD or None,
        )
        result = {{
            "command": command,
            "stdout": proc.stdout,
            "stderr": proc.stderr,
            "exit_code": proc.returncode,
        }}
    except subprocess.TimeoutExpired:
        result = {{
            "command": command,
            "stdout": "",
            "stderr": "timeout",
            "exit_code": -1,
        }}
    except Exception as exc:
        result = {{
            "command": command,
            "stdout": "",
            "stderr": str(exc),
            "exit_code": -1,
        }}
    _MANIFEST["shells"].append(result)
    return result

import builtins as _builtins_mod
_real_import = _builtins_mod.__import__
_real_open = _builtins_mod.open
_real_io_open = io.open
_real_path_open = pathlib.Path.open

def _is_write_mode(mode):
    text = str(mode or "r")
    return any(flag in text for flag in ("w", "a", "x", "+"))

def _guarded_open(file, mode="r", *args, **kwargs):
    if _DISABLE_CODEACT_FILE_EDITS and _is_write_mode(mode):
        raise RuntimeError(_CODEACT_FILE_EDIT_POLICY_MESSAGE)
    return _real_open(file, mode, *args, **kwargs)

def _guarded_io_open(file, mode="r", *args, **kwargs):
    if _DISABLE_CODEACT_FILE_EDITS and _is_write_mode(mode):
        raise RuntimeError(_CODEACT_FILE_EDIT_POLICY_MESSAGE)
    return _real_io_open(file, mode, *args, **kwargs)

def _guarded_path_open(self, mode="r", *args, **kwargs):
    if _DISABLE_CODEACT_FILE_EDITS and _is_write_mode(mode):
        raise RuntimeError(_CODEACT_FILE_EDIT_POLICY_MESSAGE)
    return _real_path_open(self, mode, *args, **kwargs)

def _guarded_import(name, *args, **kwargs):
    top = name.split(".")[0]
    if top in _BLOCKED_MODULES:
        raise ImportError(
            f"import {{name!r}} is blocked in codeact. "
            "Use daytona_codeact shell mode for commands and read() for file reads; "
            "use Daytona edit/write tools for file changes."
        )
    return _real_import(name, *args, **kwargs)

_sandbox_builtins = dict(vars(_builtins_mod))
_sandbox_builtins["__import__"] = _guarded_import
_sandbox_builtins["open"] = _guarded_open

_real_import_module = importlib.import_module

def _guarded_import_module(name, package=None):
    top = name.split(".")[0]
    if top in _BLOCKED_MODULES:
        raise ImportError(
            f"import {{name!r}} is blocked in codeact. "
            "Use daytona_codeact shell mode for commands and read() for file reads; "
            "use Daytona edit/write tools for file changes."
        )
    return _real_import_module(name, package)

importlib.import_module = _guarded_import_module

def _blocked_file_edit_call(*args, **kwargs):
    raise RuntimeError(_CODEACT_FILE_EDIT_POLICY_MESSAGE)

if _DISABLE_CODEACT_FILE_EDITS:
    for _name in (
        "remove",
        "unlink",
        "rename",
        "replace",
        "mkdir",
        "makedirs",
        "rmdir",
        "removedirs",
        "chmod",
        "chown",
        "truncate",
    ):
        if hasattr(os, _name):
            setattr(os, _name, _blocked_file_edit_call)
    for _name in (
        "write_text",
        "write_bytes",
        "touch",
        "unlink",
        "rename",
        "replace",
        "mkdir",
        "chmod",
    ):
        if hasattr(pathlib.Path, _name):
            setattr(pathlib.Path, _name, _blocked_file_edit_call)
    pathlib.Path.open = _guarded_path_open
    io.open = _guarded_io_open

if _ENFORCE_TEAM_SHELL_POLICY:
    def _blocked_os_process(*args, **kwargs):
        raise RuntimeError(
            "CodeAct policy error: coordinated team lanes must use `daytona_codeact` shell mode "
            "or `shell(\"...\")` inside Python mode for repo commands. Replace `os.system()`/"
            "`os.popen()` wrappers."
        )

    os.system = _blocked_os_process
    os.popen = _blocked_os_process

try:
    _CODE = base64.b64decode("{code_b64}").decode("utf-8")
    exec(
        _CODE,
        {{"read": read, "write": write, "shell": shell, "__name__": "__codeact__", "__builtins__": _sandbox_builtins}},
    )
except Exception:
    _MANIFEST["status"] = "error"
    _MANIFEST["error"] = traceback.format_exc()[:2000]

with open("/tmp/codeact-{run_id}.json", "w", encoding="utf-8") as f:
    json.dump(_MANIFEST, f)

print(json.dumps({{"manifest": "/tmp/codeact-{run_id}.json", "status": _MANIFEST["status"]}}))
'''


def _build_wrapper(
    code: str,
    *,
    enforce_team_shell_policy: bool,
    disable_codeact_file_edits: bool,
    run_id: str,
    cwd: str | None,
    repo_root: str | None,
) -> str:
    code_b64 = base64.b64encode(code.encode("utf-8")).decode("ascii")
    return _WRAPPER_TEMPLATE.format(
        run_id=run_id,
        code_b64=code_b64,
        codeact_cwd=_python_literal_or_none(cwd),
        codeact_repo_root=_python_literal_or_none(repo_root),
        enforce_team_shell_policy="True" if enforce_team_shell_policy else "False",
        disable_codeact_file_edits="True" if disable_codeact_file_edits else "False",
        codeact_file_edit_policy_message=json.dumps(_CODEACT_FILE_EDIT_POLICY_MESSAGE),
        shell_policy_source=shell_policy_source(),
        codeact_default_timeout=_CODEACT_DEFAULT_TIMEOUT,
    )


def _build_exec_command(script_path: str, *, cwd: str | None) -> str:
    command = f"python3 {script_path}"
    if cwd:
        command = f"cd {json.dumps(cwd)} && {command}"
    return _wrap_bash_command(command)


def _resolve_mode(
    *,
    mode: Literal["python", "shell"] | None,
    code: str | None,
    command: str | None,
) -> tuple[Literal["python", "shell"] | None, str | None]:
    has_code = isinstance(code, str) and bool(code.strip())
    has_command = isinstance(command, str) and bool(command.strip())
    if mode == "python":
        if not has_code or has_command:
            return None, "`mode=\"python\"` requires `code` and forbids `command`."
        return "python", None
    if mode == "shell":
        if not has_command or has_code:
            return None, "`mode=\"shell\"` requires `command` and forbids `code`."
        return "shell", None
    if has_code and has_command:
        return None, "Provide either `code` or `command`, not both."
    if has_code:
        return "python", None
    if has_command:
        return "shell", None
    return None, "Provide `code` for Python mode or `command` for shell mode."


async def _exec_shell_command(
    context: ToolExecutionContext,
    sandbox: object,
    *,
    command: str,
    cwd: str | None,
    timeout: int,
    attribute_changes: bool,
) -> dict[str, object]:
    wrapped_command = command if not cwd else f"cd {shlex.quote(cwd)} && {command}"
    response = await exec_ci_process_operation(
        context,
        sandbox,
        _wrap_bash_command(wrapped_command),
        timeout=timeout,
        description="daytona_codeact shell",
        attribute_changes=attribute_changes,
    )
    stdout = getattr(response, "result", "") or ""
    fallback_exit_code = getattr(response, "exit_code", None)
    cleaned_stdout, exit_code = _extract_exit_code(
        stdout,
        fallback_exit_code=fallback_exit_code,
    )
    formatted_stdout = _format_shell_stdout(cleaned_stdout, exit_code=exit_code)
    return {
        "command": command,
        "stdout": formatted_stdout,
        "stderr": formatted_stdout if exit_code != 0 else "",
        "exit_code": exit_code,
        "changed_paths": _changed_paths_from_response(response),
        "ambient_changed_paths": _ambient_changed_paths_from_response(response),
    }


async def _run_shell_with_recovery(
    context: ToolExecutionContext,
    sandbox: object,
    *,
    command: str,
    cwd: str | None,
    timeout: int,
    attribute_changes: bool,
) -> tuple[dict[str, object] | None, object, ToolResult | None]:
    try:
        return (
            await _exec_shell_command(
                context,
                sandbox,
                command=command,
                cwd=cwd,
                timeout=timeout,
                attribute_changes=attribute_changes,
            ),
            sandbox,
            None,
        )
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            return (
                await _exec_shell_command(
                    context,
                    sandbox,
                    command=command,
                    cwd=cwd,
                    timeout=timeout,
                    attribute_changes=attribute_changes,
                ),
                sandbox,
                None,
            )
        except Exception as recovery_exc:
            return None, sandbox, ToolResult(output=f"Execution failed: {recovery_exc}", is_error=True)


def _build_tool_output(
    *,
    context: ToolExecutionContext,
    status: str,
    files_written: int,
    shells: list[dict[str, object]],
    script_stdout: str,
    warnings: list[str],
    error: str = "",
) -> ToolResult:
    shell_summaries: list[str] = []
    shell_outputs: list[dict[str, object]] = []
    for shell_result in shells[:3]:
        command = str(shell_result.get("command", "") or "")
        exit_code = shell_result.get("exit_code", "?")
        try:
            exit_code_int = int(exit_code)
        except (TypeError, ValueError):
            exit_code_int = 1
        stdout = _format_shell_stdout(
            str(shell_result.get("stdout", "") or ""),
            exit_code=exit_code_int,
        )
        stderr = _format_shell_stdout(
            str(shell_result.get("stderr", "") or ""),
            exit_code=exit_code_int,
        )
        shell_summaries.append(f"$ {command[:80]} -> exit {exit_code}")
        shell_outputs.append(
            {
                "command": command,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
            }
        )

    is_error = status == "error"

    return ToolResult(
        output=json.dumps(
            {
                "cwd": _get_cwd(context) or "",
                "status": status,
                "files_written": files_written,
                "shells_run": len(shells),
                "shell_summaries": shell_summaries,
                "shell_outputs": shell_outputs,
                "script_stdout": _format_shell_stdout(script_stdout, exit_code=0),
                "warnings": warnings,
                "error": error[:500] if error else "",
            }
        ),
        is_error=is_error,
        metadata={
            "status": status,
            "files_written": files_written,
            "shells_run": len(shells),
        },
    )


async def _execute_python_wrapper(
    context: ToolExecutionContext,
    sandbox: object,
    *,
    code: str,
    cwd: str | None,
    repo_root: str | None,
    enforce_team_shell_policy: bool,
    disable_codeact_file_edits: bool,
) -> tuple[str | None, object, ToolResult | None, list[str]]:
    run_id = uuid.uuid4().hex[:8]
    wrapper = _build_wrapper(
        code,
        run_id=run_id,
        cwd=cwd,
        repo_root=repo_root,
        enforce_team_shell_policy=enforce_team_shell_policy,
        disable_codeact_file_edits=disable_codeact_file_edits,
    )
    script_path = f"/tmp/codeact-wrapper-{run_id}.py"
    exec_command = _build_exec_command(script_path, cwd=cwd)
    try:
        await _write_text_file_via_exec(
            sandbox,
            script_path,
            wrapper,
            timeout=_CODEACT_WRITE_TIMEOUT,
        )
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            try:
                await _write_text_file_via_exec(
                    sandbox,
                    script_path,
                    wrapper,
                    timeout=_CODEACT_WRITE_TIMEOUT,
                )
            except Exception:
                if _supports_exec_transport(sandbox):
                    raise
                await _upload_file_compat(sandbox, wrapper.encode("utf-8"), script_path)
        except Exception as recovery_exc:
            return (
                None,
                sandbox,
                ToolResult(
                    output=f"Failed to upload script: {recovery_exc}",
                    is_error=True,
                ),
                [],
            )

    try:
        response = await exec_ci_process_operation(
            context,
            sandbox,
            exec_command,
            timeout=_CODEACT_DEFAULT_TIMEOUT,
            description="daytona_codeact python",
        )
        return (
            getattr(response, "result", "") or "",
            sandbox,
            None,
            _changed_paths_from_response(response),
        )
    except Exception as exc:
        try:
            sandbox = await _recover_sandbox(context, exc)
            response = await exec_ci_process_operation(
                context,
                sandbox,
                exec_command,
                timeout=_CODEACT_DEFAULT_TIMEOUT,
                description="daytona_codeact python",
            )
            return (
                getattr(response, "result", "") or "",
                sandbox,
                None,
                _changed_paths_from_response(response),
            )
        except Exception as recovery_exc:
            return (
                None,
                sandbox,
                ToolResult(
                    output=f"Execution failed: {recovery_exc}",
                    is_error=True,
                ),
                [],
            )


def _ci_required_result() -> ToolResult:
    return ci_required_result(
        "daytona_codeact",
        "Command execution and Python CodeAct are disabled without CI service.",
    )


def _shell_result_error_detail(shell_result: dict[str, object]) -> str:
    return str(shell_result.get("stderr", "") or shell_result.get("stdout", "") or "")


def _changed_paths_from_response(response: object) -> list[str]:
    raw = getattr(response, "changed_paths", None)
    if not isinstance(raw, list):
        return []
    return sorted({str(path) for path in raw if str(path or "").strip()})


def _ambient_changed_paths_from_response(response: object) -> list[str]:
    raw = getattr(response, "ambient_changed_paths", None)
    if not isinstance(raw, list):
        return []
    return sorted({str(path) for path in raw if str(path or "").strip()})


def _changed_paths_from_shell(shell_result: dict[str, object]) -> list[str]:
    raw = shell_result.get("changed_paths")
    if not isinstance(raw, list):
        return []
    return sorted({str(path) for path in raw if str(path or "").strip()})


def _ambient_changed_paths_from_shell(shell_result: dict[str, object]) -> list[str]:
    raw = shell_result.get("ambient_changed_paths")
    if not isinstance(raw, list):
        return []
    return sorted({str(path) for path in raw if str(path or "").strip()})


def _ambient_change_warning(paths: list[str]) -> str:
    rendered = ", ".join(paths[:5])
    if len(paths) > 5:
        rendered += f", ... ({len(paths)} total)"
    return (
        "Workspace changed during this shell command, but coordinated CodeAct "
        "shell commands are runtime-only; treating changed paths as ambient "
        f"concurrent edits: {rendered}"
    )


def _audited_write_policy(
    context: ToolExecutionContext,
    changed_paths: list[str],
) -> tuple[list[str], str]:
    warnings: list[str] = []
    errors: list[str] = []
    for path in changed_paths:
        error = _team_repo_write_error(
            context,
            path,
            tool_name="daytona_codeact",
        )
        if error is not None:
            errors.append(error)
            continue
        warning = _team_repo_write_warning(
            context,
            path,
            tool_name="daytona_codeact",
        )
        if warning is not None:
            warnings.append(warning)
    return warnings, "\n".join(errors)


def _files_written_count(
    manifest_writes: list[object],
    changed_paths: list[str],
) -> int:
    if not manifest_writes:
        return len(changed_paths)

    manifest_paths = {
        str(item.get("path") or "")
        for item in manifest_writes
        if isinstance(item, dict) and str(item.get("path") or "").strip()
    }
    audited_only = [path for path in changed_paths if path not in manifest_paths]
    return len(manifest_writes) + len(audited_only)


@tool(
    name="daytona_codeact",
    description=(
        "Execute either Python code or a direct shell command in the Daytona sandbox. "
        "Use `command` for tests, builds, and verification; use `code` for multi-step "
        "Python with read()/shell() helpers. Do not use CodeAct for file edits; "
        "use daytona_edit_file, daytona_write_file, daytona_rename_symbol, "
        "daytona_delete_file, or daytona_move_file instead. "
        "Never include shell or Python cleanup/mutation tokens such as `rm`, `mv`, "
        "`unlink`, `os.remove`, `Path.unlink`, `shutil.rmtree`, `shutil.move`, "
        "`os.rename`, `git rm`, or `git mv`; repo deletions and path moves must use "
        "daytona_delete_file/daytona_move_file. "
        "stdout and stderr are already "
        "captured; do not append shell capture plumbing such as `2>&1` or `2>/dev/null`. "
        "Coordinated team commands already run from the repo root, so do not "
        "prefix them with `cd /testbed &&` or another repo-root cd."
    ),
    short_description="Run shell commands or Python in the sandbox.",
    input_model=DaytonaCodeActInput,
    output_model=DaytonaCodeActOutput,
    background="optional",
)
async def daytona_codeact(
    mode: Literal["python", "shell"] | None = None,
    code: str | None = None,
    command: str | None = None,
    timeout: int = _CODEACT_DEFAULT_TIMEOUT,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Execute shell commands or Python code in the Daytona sandbox."""
    resolved_mode, mode_error = _resolve_mode(mode=mode, code=code, command=command)
    if mode_error is not None:
        return ToolResult(output=mode_error, is_error=True)

    assert resolved_mode is not None

    repo_cwd = _get_cwd(context)
    disable_codeact_file_edits = _enforce_codeact_file_edit_policy(context)

    # Pre-flight policy (shell normalization, destructive-git/shell blocks,
    # file-edit side-channel blocks) is enforced by pre-phase tool guards —
    # see tools.daytona_toolkit.guards. The in-sandbox wrapper applies the
    # same checks in a second line of defense inside the sandbox process.
    if resolved_mode == "shell":
        direct_command = command or ""
        normalization_warnings: list[str] = list(
            context.metadata.get("guard_pre_warnings") or []
        )

    try:
        sandbox = await _require_sandbox(context)
    except Exception as exc:
        return ToolResult(output=str(exc), is_error=True)

    if get_ci_service(context) is None:
        return _ci_required_result()

    if resolved_mode == "shell":
        shell_result, sandbox, tool_error = await _run_shell_with_recovery(
            context,
            sandbox,
            command=direct_command,
            cwd=repo_cwd,
            timeout=timeout,
            attribute_changes=not disable_codeact_file_edits,
        )
        if tool_error is not None:
            return tool_error
        assert shell_result is not None
        exit_code = int(shell_result.get("exit_code", 1))
        changed_paths = _changed_paths_from_shell(shell_result)
        ambient_changed_paths = _ambient_changed_paths_from_shell(shell_result)
        policy_warnings, policy_error = _audited_write_policy(context, changed_paths)
        ambient_warnings = (
            [_ambient_change_warning(ambient_changed_paths)] if ambient_changed_paths else []
        )
        return _build_tool_output(
            context=context,
            status="ok" if exit_code == 0 and not policy_error else "error",
            files_written=len(changed_paths),
            shells=[shell_result],
            script_stdout="",
            warnings=list(normalization_warnings) + policy_warnings + ambient_warnings,
            error=(
                policy_error
                or (_shell_result_error_detail(shell_result) if exit_code != 0 else "")
            ),
        )

    stdout, sandbox, tool_error, changed_paths = await _execute_python_wrapper(
        context,
        sandbox,
        code=code or "",
        cwd=repo_cwd,
        repo_root=repo_cwd,
        enforce_team_shell_policy=is_coordinated_team_agent(context),
        disable_codeact_file_edits=disable_codeact_file_edits,
    )
    if tool_error is not None:
        return tool_error
    assert stdout is not None

    stdout, _ = _extract_exit_code(stdout, fallback_exit_code=0)
    stdout_lines = stdout.splitlines()
    script_stdout = "\n".join(stdout_lines[:-1]).strip() if stdout_lines else ""
    try:
        result_line = stdout_lines[-1] if stdout_lines else "{}"
        result = json.loads(result_line)
    except (json.JSONDecodeError, IndexError):
        return _build_tool_output(
            context=context,
            status="unknown",
            files_written=0,
            shells=[],
            script_stdout=stdout[:4000],
            warnings=["CodeAct result line was not valid JSON."],
        )

    manifest_path = str(result.get("manifest", "") or "")
    if not manifest_path:
        if result.get("status") == "error":
            return ToolResult(
                output=f"CodeAct execution error:\n{stdout[:4000]}",
                is_error=True,
            )
        return _build_tool_output(
            context=context,
            status="unknown",
            files_written=0,
            shells=[],
            script_stdout=stdout[:4000],
            warnings=["CodeAct wrapper did not return a manifest path."],
        )

    try:
        manifest_text, _ = await _read_text_file_via_exec(sandbox, manifest_path)
        manifest = json.loads(manifest_text)
    except Exception:
        if result.get("status") == "error":
            return ToolResult(
                output=_format_codeact_error(stdout=stdout),
                is_error=True,
            )
        return _build_tool_output(
            context=context,
            status="unknown",
            files_written=0,
            shells=[],
            script_stdout=stdout[:4000],
            warnings=["CodeAct completed but its manifest could not be read."],
        )

    shells = list(manifest.get("shells", []) or [])
    if result.get("status") == "error":
        manifest_error = str(manifest.get("error", "") or "")
        return ToolResult(
            output=_format_codeact_error(stdout=stdout, manifest_error=manifest_error),
            is_error=True,
            metadata={
                "status": manifest.get("status", "error"),
                "shells_run": len(shells),
            },
        )

    warnings = [str(w) for w in (manifest.get("warnings", []) or [])]
    writes = list(manifest.get("writes", []) or [])
    policy_warnings, policy_error = _audited_write_policy(context, changed_paths)
    return _build_tool_output(
        context=context,
        status="error" if policy_error else "ok",
        files_written=_files_written_count(writes, changed_paths),
        shells=shells,
        script_stdout=script_stdout,
        warnings=warnings + policy_warnings,
        error=policy_error or str(manifest.get("error", "") or ""),
    )
