"""CodeAct tool — multi-step code thinking and execution in a sandbox.

Executes a Python script in the sandbox with atomic OCC-guarded file I/O.
The script has access to read(), write(), and shell() helpers. All writes
are staged and committed atomically after the script finishes.
"""

from __future__ import annotations

import base64
import json
import logging
import uuid

from pydantic import BaseModel, Field

from ephemeralos.tools.base import BaseTool, ToolExecutionContext, ToolResult
from ephemeralos.tools.daytona_toolkit.ci_integration import (
    get_ci_gateway,
    prime_cache_after_write,
    record_edit_in_ledger,
)

logger = logging.getLogger(__name__)

_WRAPPER_TEMPLATE = r'''
import base64, hashlib, json, os, subprocess, sys, traceback

_RUN_ID = "{run_id}"
_MANIFEST = {{"reads": [], "writes": [], "shells": [], "status": "ok", "error": ""}}

def read(path):
    """Read a file and track the read."""
    with open(path, "r") as f:
        content = f.read()
    h = hashlib.sha256(content.encode()).hexdigest()[:16]
    _MANIFEST["reads"].append({{"path": path, "hash": h}})
    return content

def write(path, content):
    """Stage a file write (not written to disk until commit)."""
    _MANIFEST["writes"].append({{"path": path, "content": content}})

def shell(command, timeout=300):
    """Execute a shell command."""
    try:
        proc = subprocess.run(command, shell=True, capture_output=True, text=True, timeout=timeout)
        result = {{"command": command, "stdout": proc.stdout[:8000], "stderr": proc.stderr[:2000], "exit_code": proc.returncode}}
    except subprocess.TimeoutExpired:
        result = {{"command": command, "stdout": "", "stderr": "timeout", "exit_code": -1}}
    except Exception as e:
        result = {{"command": command, "stdout": "", "stderr": str(e), "exit_code": -1}}
    _MANIFEST["shells"].append(result)
    return result

try:
    _CODE = base64.b64decode("{code_b64}").decode("utf-8")
    exec(_CODE, {{"read": read, "write": write, "shell": shell, "__name__": "__codeact__"}})
except Exception as e:
    _MANIFEST["status"] = "error"
    _MANIFEST["error"] = traceback.format_exc()[:2000]

# Write manifest
with open("/tmp/codeact-{run_id}.json", "w") as f:
    json.dump(_MANIFEST, f)

print(json.dumps({{"manifest": "/tmp/codeact-{run_id}.json", "status": _MANIFEST["status"]}}))
'''


class DaytonaCodeActInput(BaseModel):
    """Arguments for CodeAct execution."""

    code: str = Field(
        description=(
            "Python code to execute in the sandbox. Has access to:\n"
            "- read(path) → str: Read a file\n"
            "- write(path, content): Stage a file write\n"
            "- shell(command, timeout=300) → dict: Run a shell command\n"
            "All writes are staged and committed atomically after execution."
        ),
    )


class DaytonaCodeActTool(BaseTool):
    """Execute multi-step code with atomic file I/O in the Daytona sandbox."""

    name = "daytona_codeact"
    description = (
        "Execute Python code in the sandbox with atomic file operations. "
        "Use read(), write(), and shell() helpers. Writes are committed "
        "atomically after execution completes successfully."
    )
    input_model = DaytonaCodeActInput

    async def execute(
        self, arguments: DaytonaCodeActInput, context: ToolExecutionContext,
    ) -> ToolResult:
        sandbox = context.metadata.get("daytona_sandbox")
        if sandbox is None:
            return ToolResult(output="No Daytona sandbox in context.", is_error=True)

        run_id = uuid.uuid4().hex[:8]
        code_b64 = base64.b64encode(arguments.code.encode("utf-8")).decode("ascii")

        # Build and upload wrapper script
        wrapper = _WRAPPER_TEMPLATE.format(run_id=run_id, code_b64=code_b64)
        script_path = f"/tmp/codeact-wrapper-{run_id}.py"

        try:
            sandbox.fs.upload_file(script_path, wrapper.encode("utf-8"))
        except Exception as exc:
            return ToolResult(output=f"Failed to upload script: {exc}", is_error=True)

        # Execute
        try:
            response = sandbox.process.exec(
                f"python3 {script_path}",
                timeout=300,
            )
            stdout = response.result or ""
        except Exception as exc:
            return ToolResult(output=f"Execution failed: {exc}", is_error=True)

        # Parse output
        try:
            result_line = stdout.strip().splitlines()[-1] if stdout.strip() else "{}"
            result = json.loads(result_line)
        except (json.JSONDecodeError, IndexError):
            return ToolResult(
                output=f"Script output:\n{stdout[:4000]}",
                metadata={"status": "unknown"},
            )

        if result.get("status") == "error":
            return ToolResult(
                output=f"CodeAct execution error:\n{stdout[:4000]}",
                is_error=True,
            )

        # Read manifest
        manifest_path = result.get("manifest", "")
        if not manifest_path:
            return ToolResult(output=f"Script output:\n{stdout[:4000]}")

        try:
            raw = sandbox.fs.download_file(manifest_path)
            manifest = json.loads(raw.decode("utf-8") if isinstance(raw, bytes) else raw)
        except Exception:
            return ToolResult(output=f"Script completed but manifest unreadable:\n{stdout[:4000]}")

        # Commit staged writes
        writes = manifest.get("writes", [])
        committed = 0
        errors = []

        for w in writes:
            path = w.get("path", "")
            content = w.get("content", "")
            try:
                sandbox.fs.upload_file(path, content.encode("utf-8"))
                prime_cache_after_write(context, path, content)
                record_edit_in_ledger(context, path, edit_type="codeact")
                committed += 1
            except Exception as exc:
                errors.append(f"{path}: {exc}")

        # Build output
        output_parts = []
        if manifest.get("status") == "ok":
            output_parts.append(f"CodeAct completed: {committed} file(s) written")
        else:
            output_parts.append(f"CodeAct finished with status: {manifest.get('status')}")

        if errors:
            output_parts.append(f"Write errors: {'; '.join(errors)}")

        shells = manifest.get("shells", [])
        if shells:
            output_parts.append(f"Shell commands executed: {len(shells)}")
            for sh in shells[:3]:
                cmd = sh.get("command", "")[:80]
                exit_code = sh.get("exit_code", "?")
                output_parts.append(f"  $ {cmd} → exit {exit_code}")

        if manifest.get("error"):
            output_parts.append(f"Error: {manifest['error'][:500]}")

        return ToolResult(
            output="\n".join(output_parts),
            is_error=bool(errors),
            metadata={
                "status": manifest.get("status", "unknown"),
                "files_written": committed,
                "shells_run": len(shells),
            },
        )
