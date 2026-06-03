#!/usr/bin/env python3
"""Live Rust daemon benchmark for a generic process-backed plugin service.

This reuses an existing Docker sandbox when ``--container-id`` is provided. It
uploads the packaged Rust ``eosd`` binary, starts ``EOS_SANDBOX_RUNTIME=rust``,
then proves a vanilla plugin service can:

* connect through the daemon-managed PPC socket,
* answer a read-only ``plugin.generic.ping`` request, and
* publish a self-managed ``plugin.generic.apply`` write through the daemon's
  generic ``daemon.occ.apply_changeset`` callback, and
* publish a self-managed ``plugin.generic.apply_multi`` write through two
  daemon-managed OCC callbacks before the plugin's final reply, and
* fail closed when one service rejects a daemon health probe without poisoning
  unrelated services, and
* recover that same failed-health service on the next dispatch through another
  route, and
* restart a second read-only ``workspace_snapshot_refresh`` service through
  the generic ``restart_service`` fallback after a peer publish, and
* run a vanilla one-shot ``plugin.generic.oneshot_write`` worker through the
  daemon-owned workspace overlay/OCC path.
* resolve a real Pyright LSP ``textDocument/definition`` response through the
  refreshed read-only service path.
* resolve a real Pyright LSP ``textDocument/declaration`` response through the
  same refreshed read-only service path.
* resolve a real Pyright LSP ``textDocument/completion`` response through the
  same refreshed read-only service path.
* resolve a real Pyright LSP ``completionItem/resolve`` response through the
  same refreshed read-only service path.
* consume a real Pyright LSP ``textDocument/publishDiagnostics`` notification
  through the same refreshed read-only service path.
* resolve a real Pyright LSP ``textDocument/signatureHelp`` response through
  the same refreshed read-only service path.
* resolve a real Pyright LSP ``workspace/symbol`` response through the same
  refreshed read-only service path.
* resolve a real Pyright LSP ``textDocument/hover`` response through the same
  refreshed read-only service path.
* resolve a real Pyright LSP ``textDocument/typeDefinition`` response through
  the same refreshed read-only service path.
* resolve real Pyright LSP ``textDocument/prepareCallHierarchy`` responses and
  call-hierarchy incoming/outgoing calls through the same refreshed read-only
  service path.
* resolve a real Pyright LSP ``textDocument/documentHighlight`` response through
  the same refreshed read-only service path.
* resolve a real Pyright LSP ``textDocument/prepareRename`` response through
  the same refreshed read-only service path.
* resolve a real Pyright LSP ``textDocument/references`` response through the
  same refreshed read-only service path.
* compute a real Pyright LSP ``textDocument/rename`` edit and publish it
  through the daemon's self-managed OCC callback path.
* apply a generic positive LSP ``textDocument/formatting`` edit through the
  daemon's self-managed OCC callback path, independent of Pyright's unsupported
  formatting boundary.
* fail closed when a dedicated service process exits during a connected route.
* recover that crashed service on the next dispatch through another route on
  the same service.
* recover a previously ready read-only service on the next dispatch after a
  PPC/process failure.
* launch the reusable bundled Python PPC service bridge for an installed
  plugin module and publish mounted workspace changes through its generic OCC
  callback.
* publish canonical importlib LSP bridge formatting and execute-command shaped
  writes through the reusable PPC service bridge.

Workspace experiment files live under ``/eos/plugin/*``; the reusable bridge
plugin module is installed under the daemon runtime bundle catalog.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import platform
import shlex
import sys
import time
import uuid
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
BACKEND_SRC = ROOT / "backend" / "src"
SCRIPT_DIR = Path(__file__).resolve().parent
if str(BACKEND_SRC) not in sys.path:
    sys.path.insert(0, str(BACKEND_SRC))
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from bench_rust_daemon_phase2 import (  # noqa: E402
    reset_runtime,
    result_block,
    temporary_env,
    upload_artifact,
)
from bench_sandbox_e2e import (  # noqa: E402
    DEFAULT_DOCKER_IMAGE,
    DockerBench,
    collect_environment,
    elapsed_ms,
    tar_file_at_path,
)

PLUGIN_ROOT = "/eos/plugin"
BUNDLE_REMOTE_DIR = "/eos/daemon"
PPC_SERVICE_BUNDLE_FILES = (
    "plugins/__init__.py",
    "plugins/catalog/lsp/runtime/__init__.py",
    "plugins/catalog/lsp/runtime/apply.py",
    "plugins/catalog/lsp/runtime/lsp_jsonrpc.py",
    "plugins/catalog/lsp/runtime/pyright_session.py",
    "plugins/catalog/lsp/runtime/server.py",
    "plugins/catalog/lsp/runtime/session_manager.py",
    "sandbox/__init__.py",
    "sandbox/shared/__init__.py",
    "sandbox/shared/models.py",
    "sandbox/ephemeral_workspace/__init__.py",
    "sandbox/ephemeral_workspace/plugin/__init__.py",
    "sandbox/ephemeral_workspace/plugin/op_context.py",
    "sandbox/ephemeral_workspace/plugin/op_registry.py",
    "sandbox/ephemeral_workspace/plugin/ppc_service.py",
)
LAYER_STACK_ROOT = f"{PLUGIN_ROOT}/rust-layer-stack"
WORKSPACE_ROOT = f"{PLUGIN_ROOT}/rust-workspace"
ISOLATED_SCRATCH_ROOT = f"{PLUGIN_ROOT}/iws-scratch"
HARNESS_SCRIPT = f"{PLUGIN_ROOT}/rust_ppc_harness.py"
HARNESS_LOG = f"{PLUGIN_ROOT}/rust_ppc_harness.jsonl"
RECOVER_MARKER = f"{PLUGIN_ROOT}/recover_probe_once.flag"
ONESHOT_SCRIPT = f"{PLUGIN_ROOT}/rust_oneshot_worker.py"
VANILLA_PACKAGE_SCRIPT = f"{PLUGIN_ROOT}/rust_vanilla_package.py"
PYRIGHT_SETUP_SCRIPT = f"{PLUGIN_ROOT}/rust_pyright_setup.sh"
RUNTIME_BRIDGE_PLUGIN_ROOT = f"{BUNDLE_REMOTE_DIR}/plugins/catalog/generic"
RUNTIME_BRIDGE_SERVER = f"{RUNTIME_BRIDGE_PLUGIN_ROOT}/runtime/server.py"
AGENT_ID = "rust-daemon-plugin-bench"
TARGET_REL = "live_plugin_result.txt"
TARGET_CONTENT = "from live rust plugin\n"
RUNTIME_BRIDGE_TARGET_REL = "live_plugin_runtime_bridge.txt"
RUNTIME_BRIDGE_CONTENT = "from reusable ppc bridge\n"
RUNTIME_BRIDGE_CONCURRENT_A_REL = "live_plugin_runtime_bridge_concurrent_a.txt"
RUNTIME_BRIDGE_CONCURRENT_A_CONTENT = "from concurrent runtime bridge a\n"
RUNTIME_BRIDGE_CONCURRENT_B_REL = "live_plugin_runtime_bridge_concurrent_b.txt"
RUNTIME_BRIDGE_CONCURRENT_B_CONTENT = "from concurrent runtime bridge b\n"
LSP_BRIDGE_TARGET_REL = "live_plugin_lsp_bridge.py"
LSP_BRIDGE_CONTENT = "def bridge_value() -> int:\n    return 7\n\nRESULT = bridge_value()\n"
LSP_BRIDGE_SYMBOL = "bridge_value"
LSP_BRIDGE_RENAMED_SYMBOL = "bridge_total"
LSP_BRIDGE_RENAMED_CONTENT = LSP_BRIDGE_CONTENT.replace(
    LSP_BRIDGE_SYMBOL,
    LSP_BRIDGE_RENAMED_SYMBOL,
)
LSP_BRIDGE_APPLY_TARGET_REL = "live_plugin_lsp_bridge_apply.py"
LSP_BRIDGE_APPLY_CONTENT = "VALUE = 'before'\n"
LSP_BRIDGE_APPLY_CONTENT_AFTER = "VALUE = 'after'\n"
LSP_BRIDGE_CODE_ACTION_TARGET_REL = "live_plugin_lsp_bridge_code_action.py"
LSP_BRIDGE_CODE_ACTION_CONTENT = "status = 'before'\n"
LSP_BRIDGE_CODE_ACTION_CONTENT_AFTER = "status = 'after'\n"
LSP_BRIDGE_CODE_ACTION_TITLE = "Bridge replace status"
LSP_BRIDGE_CODE_ACTION_KIND = "quickfix"
LSP_BRIDGE_FORMAT_TARGET_REL = "live_plugin_lsp_bridge_format.py"
LSP_BRIDGE_FORMAT_CONTENT = "def bridge_format():\n    return    2\n"
LSP_BRIDGE_FORMAT_CONTENT_AFTER = "def bridge_format() -> int:\n    return 2\n"
LSP_BRIDGE_EXECUTE_COMMAND_TARGET_REL = "live_plugin_lsp_bridge_execute_command.py"
LSP_BRIDGE_EXECUTE_COMMAND_CONTENT = "value = 'before-bridge'\n"
LSP_BRIDGE_EXECUTE_COMMAND_CONTENT_AFTER = "value = 'after-bridge'\n"
LSP_BRIDGE_EXECUTE_COMMAND_NAME = "generic.applyWorkspaceEdit"
LSP_BRIDGE_DIAGNOSTICS_TARGET_REL = "live_plugin_lsp_bridge_diagnostics.py"
LSP_BRIDGE_DIAGNOSTICS_CONTENT = "value: List[int] = []\n"
LSP_BRIDGE_DIAGNOSTICS_LINE = 0
LSP_BRIDGE_DIAGNOSTICS_CHARACTER = len("value: Li")
LSP_BRIDGE_DIAGNOSTICS_SYMBOL = "List"
MULTI_TARGET_A_REL = "live_plugin_multi_a.txt"
MULTI_TARGET_A_CONTENT = "from live rust plugin multi a\n"
MULTI_TARGET_B_REL = "live_plugin_multi_b.txt"
MULTI_TARGET_B_CONTENT = "from live rust plugin multi b\n"
SHELL_TARGET_REL = "live_plugin_shell_result.txt"
SHELL_CONTENT = "from live rust shell publish\n"
ONESHOT_TARGET_REL = "live_plugin_oneshot_result.txt"
ONESHOT_CONTENT = "from live rust oneshot plugin\n"
CRASH_RECOVERY_TARGET_REL = "live_plugin_crash_recovery.txt"
CRASH_RECOVERY_CONTENT = "from crash recovery peer publish\n"
PYRIGHT_TARGET_REL = "live_plugin_pyright.py"
PYRIGHT_CONTENT = "def live_value() -> int:\n    return 42\n\nRESULT = live_value()\n"
PYRIGHT_SYMBOL = "live_value"
PYRIGHT_COMPLETION_TARGET_REL = "live_plugin_completion.py"
PYRIGHT_COMPLETION_CONTENT = "def live_value() -> int:\n    return 42\n\nRESULT = live_\n"
PYRIGHT_COMPLETION_LINE = 3
PYRIGHT_COMPLETION_CHARACTER = len("RESULT = live_")
PYRIGHT_DIAGNOSTICS_TARGET_REL = "live_plugin_diagnostics.py"
PYRIGHT_DIAGNOSTICS_CONTENT = "value: List[int] = []\n"
PYRIGHT_DIAGNOSTICS_LINE = 0
PYRIGHT_DIAGNOSTICS_CHARACTER = len("value: Li")
PYRIGHT_DIAGNOSTICS_SYMBOL = "List"
PYRIGHT_CODE_ACTION_TARGET_REL = "live_plugin_code_actions.py"
PYRIGHT_CODE_ACTION_CONTENT = (
    "import sys\n"
    "import os\n\n"
    "VALUE = os.path.join('a', 'b')\n"
)
PYRIGHT_CODE_ACTION_LINE = 0
PYRIGHT_CODE_ACTION_CHARACTER = 0
PYRIGHT_CODE_ACTION_KIND = "source.organizeImports"
PYRIGHT_DOCUMENT_FORMATTING_METHOD = "textDocument/formatting"
PYRIGHT_DOCUMENT_FORMATTING_CAPABILITY = "documentFormattingProvider"
PYRIGHT_EXECUTE_COMMAND_METHOD = "workspace/executeCommand"
LSP_APPLY_EDIT_TARGET_REL = "live_plugin_apply_workspace_edit.py"
LSP_APPLY_EDIT_CONTENT = "alpha\nbeta\n"
LSP_APPLY_EDIT_REPLACEMENT = "edited"
LSP_APPLY_EDIT_CONTENT_AFTER = "alpha\nedited\n"
LSP_APPLY_CODE_ACTION_TARGET_REL = "live_plugin_apply_code_action.py"
LSP_APPLY_CODE_ACTION_CONTENT = "before\nunchanged\n"
LSP_APPLY_CODE_ACTION_REPLACEMENT = "after"
LSP_APPLY_CODE_ACTION_CONTENT_AFTER = "after\nunchanged\n"
LSP_APPLY_CODE_ACTION_TITLE = "Replace first line"
LSP_APPLY_CODE_ACTION_KIND = "quickfix"
LSP_FORMAT_TARGET_REL = "live_plugin_format.py"
LSP_FORMAT_CONTENT = "def format_me():\n    return    1\n"
LSP_FORMAT_CONTENT_AFTER = "def format_me() -> int:\n    return 1\n"
LSP_FORMAT_METHOD = "textDocument/formatting"
LSP_EXECUTE_COMMAND_TARGET_REL = "live_plugin_execute_command.py"
LSP_EXECUTE_COMMAND_CONTENT = "value = 'before'\n"
LSP_EXECUTE_COMMAND_CONTENT_AFTER = "value = 'after'\n"
LSP_EXECUTE_COMMAND_NAME = "generic.applyWorkspaceEdit"
LSP_EXECUTE_COMMAND_METHOD = "workspace/executeCommand"
PYRIGHT_SIGNATURE_TARGET_REL = "live_plugin_signature.py"
PYRIGHT_SIGNATURE_CONTENT = (
    "def live_signature(left: int, right: str) -> str:\n"
    '    return f"{left}:{right}"\n\n'
    "RESULT = live_signature(42, \n"
)
PYRIGHT_SIGNATURE_LINE = 3
PYRIGHT_SIGNATURE_CHARACTER = len("RESULT = live_signature(42, ")
PYRIGHT_TYPE_TARGET_REL = "live_plugin_type.py"
PYRIGHT_TYPE_CONTENT = (
    "class LiveThing:\n"
    "    pass\n\n"
    "thing = LiveThing()\n"
    "RESULT = thing\n"
)
PYRIGHT_TYPE_LINE = 4
PYRIGHT_TYPE_CHARACTER = len("RESULT = th")
PYRIGHT_TYPE_CLASS = "LiveThing"
PYRIGHT_CALL_HIERARCHY_TARGET_REL = "live_plugin_call_hierarchy.py"
PYRIGHT_CALL_HIERARCHY_CONTENT = (
    "def live_callee() -> int:\n"
    "    return 1\n\n"
    "def live_caller() -> int:\n"
    "    return live_callee()\n\n"
    "RESULT = live_caller()\n"
)
PYRIGHT_CALL_HIERARCHY_LINE = 0
PYRIGHT_CALL_HIERARCHY_CHARACTER = len("def live_ca")
PYRIGHT_CALL_HIERARCHY_SYMBOL = "live_callee"
PYRIGHT_CALL_HIERARCHY_CALLER = "live_caller"
PYRIGHT_CALL_HIERARCHY_OUTGOING_LINE = 3
PYRIGHT_CALL_HIERARCHY_OUTGOING_CHARACTER = len("def live_ca")
PYRIGHT_RENAMED_SYMBOL = "live_total"
PYRIGHT_RENAMED_CONTENT = PYRIGHT_CONTENT.replace(PYRIGHT_SYMBOL, PYRIGHT_RENAMED_SYMBOL)

HARNESS_SOURCE = r'''
from __future__ import annotations

import json
import os
import select
import shutil
import socket
import subprocess
import sys
import time
import traceback

LOG_PATH = "/eos/plugin/rust_ppc_harness.jsonl"
ADAPTER_WATCH_PATH = "live_plugin_result.txt"
VANILLA_PACKAGE_SCRIPT = "/eos/plugin/rust_vanilla_package.py"
PYRIGHT_SETUP_SCRIPT = "/eos/plugin/rust_pyright_setup.sh"
CURRENT_MANIFEST_KEY = ""
ADAPTER = None
PYRIGHT = None


def log(event: str, **fields: object) -> None:
    os.makedirs(os.path.dirname(LOG_PATH), exist_ok=True)
    payload = {"event": event, "time": time.time(), **fields}
    with open(LOG_PATH, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(payload, sort_keys=True) + "\n")


def read_frame(sock: socket.socket) -> dict[str, object]:
    chunks: list[bytes] = []
    while True:
        data = sock.recv(1)
        if not data:
            raise EOFError("PPC socket closed")
        chunks.append(data)
        if data == b"\n":
            break
    raw = b"".join(chunks)
    envelope = json.loads(raw.decode("utf-8"))
    args = envelope.get("args") or {}
    return {
        "op": envelope["op"],
        "message_id": envelope["invocation_id"],
        "direction": args["direction"],
        "body": json.loads(args.get("body") or "{}"),
    }


def write_frame(
    sock: socket.socket,
    *,
    op: str,
    message_id: str,
    direction: str,
    body: dict[str, object],
) -> None:
    envelope = {
        "op": op,
        "invocation_id": message_id,
        "args": {
            "direction": direction,
            "body": json.dumps(body, separators=(",", ":"), sort_keys=True),
        },
    }
    sock.sendall(json.dumps(envelope, separators=(",", ":")).encode("utf-8") + b"\n")


def reply(sock: socket.socket, request: dict[str, object], body: dict[str, object]) -> None:
    write_frame(
        sock,
        op="reply",
        message_id=str(request["message_id"]),
        direction="reply",
        body=body,
    )


def send_occ_callback(
    sock: socket.socket,
    request: dict[str, object],
    *,
    suffix: str,
    changes: list[dict[str, object]],
) -> dict[str, object]:
    callback_id = f"{request['message_id']}:{suffix}"
    write_frame(
        sock,
        op="daemon.occ.apply_changeset",
        message_id=callback_id,
        direction="request",
        body={
            "layer_stack_root": os.environ["EOS_PLUGIN_LAYER_STACK_ROOT"],
            "changes": changes,
        },
    )
    callback_reply = read_frame(sock)
    if callback_reply["direction"] != "reply":
        raise RuntimeError("OCC callback did not return a reply frame")
    if callback_reply["message_id"] != callback_id:
        raise RuntimeError("OCC callback reply message_id mismatch")
    callback_body = callback_reply["body"]
    if not isinstance(callback_body, dict):
        raise RuntimeError("OCC callback reply body was not an object")
    return callback_body


class PackageAdapter:
    def __init__(self) -> None:
        self.proc = subprocess.Popen(
            ["python3", VANILLA_PACKAGE_SCRIPT],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
        )

    def request(self, payload: dict[str, object]) -> dict[str, object]:
        if self.proc.stdin is None or self.proc.stdout is None:
            raise RuntimeError("vanilla package pipes are unavailable")
        self.proc.stdin.write(json.dumps(payload, sort_keys=True) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            stderr = ""
            if self.proc.stderr is not None:
                stderr = self.proc.stderr.read()
            raise RuntimeError(f"vanilla package closed stdout: {stderr}")
        reply_payload = json.loads(line)
        if not isinstance(reply_payload, dict):
            raise RuntimeError("vanilla package reply was not an object")
        return reply_payload

    def load(self, path: str) -> dict[str, object]:
        return self.request(
            {
                "op": "load",
                "workspace_root": os.environ["EOS_PLUGIN_WORKSPACE_ROOT"],
                "path": path,
            }
        )

    def query(self, path: str) -> dict[str, object]:
        return self.request({"op": "query", "path": path})

    def close(self) -> None:
        try:
            self.request({"op": "shutdown"})
        except Exception:
            pass
        try:
            self.proc.terminate()
            self.proc.wait(timeout=1)
        except Exception:
            self.proc.kill()


class PyrightAdapter:
    def __init__(self) -> None:
        self.proc: subprocess.Popen[bytes] | None = None
        self.buffer = b""
        self.next_id = 1
        self.opened_versions: dict[str, int] = {}
        self.initialize_result: dict[str, object] = {}
        self.server_capabilities: dict[str, object] = {}

    def ensure_started(self) -> None:
        if self.proc is not None and self.proc.poll() is None:
            return
        env = os.environ.copy()
        env["PATH"] = "/tmp/eos-node22/bin:" + env.get("PATH", "")
        setup = subprocess.run(
            [PYRIGHT_SETUP_SCRIPT],
            capture_output=True,
            text=True,
            timeout=180,
            check=False,
            env=env,
        )
        log(
            "pyright_setup",
            returncode=setup.returncode,
            stdout=setup.stdout[-1000:],
            stderr=setup.stderr[-1000:],
        )
        if setup.returncode != 0:
            raise RuntimeError(f"pyright setup failed: {setup.stderr[-1000:]}")
        binary = shutil.which("pyright-langserver", path=env["PATH"])
        if not binary:
            raise RuntimeError("pyright-langserver not found after setup")
        self.proc = subprocess.Popen(
            [binary, "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        root_uri = file_uri(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"])
        initialize_result = self.request(
            "initialize",
            {
                "processId": None,
                "rootUri": root_uri,
                "workspaceFolders": [
                    {
                        "uri": root_uri,
                        "name": "rust-plugin-live-workspace",
                    }
                ],
                "capabilities": {
                    "textDocument": {
                        "documentSymbol": {"hierarchicalDocumentSymbolSupport": True},
                        "completion": {
                            "completionItem": {
                                "labelDetailsSupport": True,
                                "snippetSupport": False,
                            }
                        },
                        "signatureHelp": {
                            "signatureInformation": {
                                "parameterInformation": {"labelOffsetSupport": True},
                            }
                        },
                        "typeDefinition": {"dynamicRegistration": False},
                        "codeAction": {
                            "dynamicRegistration": False,
                            "codeActionLiteralSupport": {
                                "codeActionKind": {
                                    "valueSet": [
                                        "",
                                        "quickfix",
                                        "source.organizeImports",
                                    ]
                                }
                            },
                        },
                        "documentHighlight": {"dynamicRegistration": False},
                        "synchronization": {"didSave": True},
                    },
                    "workspace": {
                        "didChangeWatchedFiles": {"dynamicRegistration": False},
                        "symbol": {"dynamicRegistration": False},
                    },
                },
            },
            timeout_s=30,
        )
        self.initialize_result = (
            initialize_result if isinstance(initialize_result, dict) else {}
        )
        raw_capabilities = self.initialize_result.get("capabilities")
        self.server_capabilities = (
            raw_capabilities if isinstance(raw_capabilities, dict) else {}
        )
        self.notify("initialized", {})
        log("pyright_started", pid=self.proc.pid, binary=binary)

    def refresh(self) -> None:
        self.opened_versions.clear()
        log("pyright_refreshed")

    def document_symbols(self, path: str, query: str) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/documentSymbol",
            {"textDocument": {"uri": uri}},
            timeout_s=30,
        )
        symbols = flatten_symbols(raw)
        if query:
            symbols = [
                symbol
                for symbol in symbols
                if query.lower() in str(symbol.get("name", "")).lower()
            ]
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "query": query,
            "symbols": symbols,
            "symbol_names": [str(symbol.get("name", "")) for symbol in symbols],
        }

    def workspace_symbols(self, query: str) -> dict[str, object]:
        self.ensure_started()
        raw = self.request(
            "workspace/symbol",
            {"query": query},
            timeout_s=30,
        )
        symbols = lsp_workspace_symbols(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], raw)
        if query:
            symbols = [
                symbol
                for symbol in symbols
                if query.lower() in str(symbol.get("name", "")).lower()
            ]
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "query": query,
            "symbols": symbols,
            "symbol_count": len(symbols),
            "symbol_names": [str(symbol.get("name", "")) for symbol in symbols],
            "symbol_paths": [str(symbol.get("path", "")) for symbol in symbols],
        }

    def capabilities(self) -> dict[str, object]:
        self.ensure_started()
        capability_keys = sorted(str(key) for key in self.server_capabilities)
        execute_command_provider = self.server_capabilities.get(
            "executeCommandProvider",
        )
        execute_commands = (
            execute_command_provider.get("commands", [])
            if isinstance(execute_command_provider, dict)
            else []
        )
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "capability_keys": capability_keys,
            "supports": {
                "completion": "completionProvider" in self.server_capabilities,
                "completion_resolve": bool(
                    isinstance(
                        self.server_capabilities.get("completionProvider"),
                        dict,
                    )
                    and self.server_capabilities.get("completionProvider", {}).get(
                        "resolveProvider"
                    )
                ),
                "hover": bool(self.server_capabilities.get("hoverProvider")),
                "signature_help": "signatureHelpProvider" in self.server_capabilities,
                "definition": bool(self.server_capabilities.get("definitionProvider")),
                "declaration": bool(
                    self.server_capabilities.get("declarationProvider")
                ),
                "type_definition": bool(
                    self.server_capabilities.get("typeDefinitionProvider")
                ),
                "document_highlight": bool(
                    self.server_capabilities.get("documentHighlightProvider")
                ),
                "document_symbol": bool(
                    self.server_capabilities.get("documentSymbolProvider")
                ),
                "workspace_symbol": bool(
                    self.server_capabilities.get("workspaceSymbolProvider")
                ),
                "references": bool(self.server_capabilities.get("referencesProvider")),
                "rename": bool(self.server_capabilities.get("renameProvider")),
                "code_action": "codeActionProvider" in self.server_capabilities,
                "call_hierarchy": bool(
                    self.server_capabilities.get("callHierarchyProvider")
                ),
                "document_formatting": bool(
                    self.server_capabilities.get("documentFormattingProvider")
                ),
                "document_range_formatting": bool(
                    self.server_capabilities.get("documentRangeFormattingProvider")
                ),
                "execute_command_provider": bool(execute_command_provider),
                "execute_command": bool(execute_commands),
                "folding_range": "foldingRangeProvider" in self.server_capabilities,
            },
            "raw": self.server_capabilities,
        }

    def document_formatting(self, path: str) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        provider = self.server_capabilities.get("documentFormattingProvider")
        if not provider:
            return {
                "protocol": "lsp-jsonrpc",
                "server": "pyright-langserver",
                "pid": self.proc.pid if self.proc is not None else None,
                "path": path,
                "method": "textDocument/formatting",
                "capability": "documentFormattingProvider",
                "supported": False,
                "unsupported": True,
                "reason": "server did not advertise documentFormattingProvider",
                "edits": [],
                "edit_count": 0,
            }
        raw = self.request(
            "textDocument/formatting",
            {
                "textDocument": {"uri": uri},
                "options": {"tabSize": 4, "insertSpaces": True},
            },
            timeout_s=30,
        )
        edits = [edit for edit in raw if isinstance(edit, dict)] if isinstance(raw, list) else []
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "method": "textDocument/formatting",
            "capability": "documentFormattingProvider",
            "supported": True,
            "unsupported": False,
            "edits": edits,
            "edit_count": len(edits),
        }

    def execute_command(self, command: str) -> dict[str, object]:
        self.ensure_started()
        provider = self.server_capabilities.get("executeCommandProvider")
        commands = (
            provider.get("commands", []) if isinstance(provider, dict) else []
        )
        if not command and commands:
            command = str(commands[0])
        if not command or command not in commands:
            return {
                "protocol": "lsp-jsonrpc",
                "server": "pyright-langserver",
                "pid": self.proc.pid if self.proc is not None else None,
                "method": "workspace/executeCommand",
                "capability": "executeCommandProvider.commands",
                "supported": False,
                "unsupported": True,
                "reason": "server did not advertise executable commands",
                "command": command,
                "commands": commands,
            }
        raw = self.request(
            "workspace/executeCommand",
            {"command": command, "arguments": []},
            timeout_s=30,
        )
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "method": "workspace/executeCommand",
            "capability": "executeCommandProvider.commands",
            "supported": True,
            "unsupported": False,
            "command": command,
            "commands": commands,
            "result": raw,
        }

    def completion(
        self,
        path: str,
        line: int,
        character: int,
        query: str,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/completion",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
                "context": {"triggerKind": 1},
            },
            timeout_s=30,
        )
        items = lsp_completion_items(raw)
        labels = [str(item.get("label", "")) for item in items]
        query_lower = query.lower()
        matching_labels = [
            label for label in labels if query_lower and query_lower in label.lower()
        ]
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "query": query,
            "items": items[:50],
            "item_count": len(items),
            "labels": labels[:100],
            "matching_labels": matching_labels,
        }

    def completion_resolve(
        self,
        path: str,
        line: int,
        character: int,
        query: str,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/completion",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
                "context": {"triggerKind": 1},
            },
            timeout_s=30,
        )
        raw_items = lsp_completion_raw_items(raw)
        query_lower = query.lower()
        selected = None
        for item in raw_items:
            label = item.get("label")
            if isinstance(label, str) and query_lower and query_lower in label.lower():
                selected = item
                break
        if selected is None:
            raise RuntimeError(f"pyright completion item not found for query {query!r}")
        resolved = self.request(
            "completionItem/resolve",
            selected,
            timeout_s=30,
        )
        if not isinstance(resolved, dict):
            raise RuntimeError("pyright completionItem/resolve returned no item")
        request_items = lsp_completion_items([selected])
        resolved_items = lsp_completion_items([resolved])
        if not request_items or not resolved_items:
            raise RuntimeError("pyright completionItem/resolve returned unlabeled item")
        request_item = request_items[0]
        resolved_item = resolved_items[0]
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "query": query,
            "request_item": request_item,
            "request_label": request_item.get("label"),
            "resolved_item": resolved_item,
            "resolved_label": resolved_item.get("label"),
            "resolved_detail": resolved.get("detail"),
            "documentation_text": lsp_markup_text(resolved.get("documentation")),
            "data_present": "data" in selected or "data" in resolved,
        }

    def signature_help(
        self,
        path: str,
        line: int,
        character: int,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/signatureHelp",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
                "context": {
                    "triggerKind": 1,
                    "isRetrigger": False,
                },
            },
            timeout_s=30,
        )
        signatures = lsp_signature_items(raw)
        labels = [str(signature.get("label", "")) for signature in signatures]
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "signatures": signatures,
            "signature_count": len(signatures),
            "labels": labels,
            "active_signature": raw.get("activeSignature") if isinstance(raw, dict) else None,
            "active_parameter": raw.get("activeParameter") if isinstance(raw, dict) else None,
        }

    def diagnostics(
        self,
        path: str,
        line: int,
        character: int,
        query: str,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        diagnostics = self.wait_for_diagnostics(uri, timeout_s=30)
        diagnostic_messages = [
            str(diagnostic.get("message", ""))
            for diagnostic in diagnostics
            if isinstance(diagnostic, dict)
        ]
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "diagnostics": diagnostics,
            "diagnostic_count": len(diagnostics),
            "diagnostic_messages": diagnostic_messages,
            "matching_messages": [
                message for message in diagnostic_messages if query in message
            ],
            "diagnostic_codes": [
                str(diagnostic.get("code", ""))
                for diagnostic in diagnostics
                if isinstance(diagnostic, dict)
            ],
        }

    def code_actions(
        self,
        path: str,
        line: int,
        character: int,
        only: list[str],
        diagnostics: list[dict[str, object]],
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        range_obj: dict[str, object] = {
            "start": {"line": line, "character": character},
            "end": {"line": line, "character": character},
        }
        if diagnostics:
            raw_range = diagnostics[0].get("range")
            if isinstance(raw_range, dict):
                range_obj = raw_range
        context: dict[str, object] = {"diagnostics": diagnostics}
        if only:
            context["only"] = only
        raw = self.request(
            "textDocument/codeAction",
            {
                "textDocument": {"uri": uri},
                "range": range_obj,
                "context": context,
            },
            timeout_s=30,
        )
        actions = [action for action in raw if isinstance(action, dict)] if isinstance(raw, list) else []
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "only": only,
            "range": range_obj,
            "diagnostic_count": len(diagnostics),
            "actions": actions[:20],
            "action_count": len(actions),
            "action_titles": [str(action.get("title", "")) for action in actions],
            "action_kinds": [str(action.get("kind", "")) for action in actions],
        }

    def hover(
        self,
        path: str,
        line: int,
        character: int,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/hover",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            },
            timeout_s=30,
        )
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "hover": raw,
            "hover_text": lsp_markup_text(raw.get("contents") if isinstance(raw, dict) else raw),
        }

    def document_highlight(
        self,
        path: str,
        line: int,
        character: int,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/documentHighlight",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            },
            timeout_s=30,
        )
        highlights = []
        if isinstance(raw, list):
            highlights = [
                {
                    "path": path,
                    "range": highlight.get("range"),
                    "kind": highlight.get("kind"),
                }
                for highlight in raw
                if isinstance(highlight, dict) and "range" in highlight
            ]
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "highlights": highlights,
            "highlight_count": len(highlights),
            "raw": raw,
        }

    def prepare_rename(
        self,
        path: str,
        line: int,
        character: int,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/prepareRename",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            },
            timeout_s=30,
        )
        range_value = None
        placeholder = None
        default_behavior = False
        if isinstance(raw, dict):
            if "range" in raw:
                range_value = raw.get("range")
                placeholder = raw.get("placeholder")
            elif "start" in raw and "end" in raw:
                range_value = raw
            default_behavior = bool(raw.get("defaultBehavior", False))
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "range": range_value,
            "placeholder": placeholder,
            "default_behavior": default_behavior,
            "raw": raw,
        }

    def definition(
        self,
        path: str,
        line: int,
        character: int,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/definition",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            },
            timeout_s=30,
        )
        locations = lsp_locations(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], raw)
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "locations": locations,
            "definition_count": len(locations),
        }

    def declaration(
        self,
        path: str,
        line: int,
        character: int,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/declaration",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            },
            timeout_s=30,
        )
        locations = lsp_locations(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], raw)
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "locations": locations,
            "declaration_count": len(locations),
        }

    def type_definition(
        self,
        path: str,
        line: int,
        character: int,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/typeDefinition",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            },
            timeout_s=30,
        )
        locations = lsp_locations(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], raw)
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "locations": locations,
            "type_definition_count": len(locations),
        }

    def call_hierarchy(
        self,
        path: str,
        line: int,
        character: int,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw_prepare = self.request(
            "textDocument/prepareCallHierarchy",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            },
            timeout_s=30,
        )
        raw_items = raw_prepare if isinstance(raw_prepare, list) else [raw_prepare]
        raw_items = [item for item in raw_items if isinstance(item, dict)]
        items = lsp_call_hierarchy_items(
            os.environ["EOS_PLUGIN_WORKSPACE_ROOT"],
            raw_items,
        )
        incoming_calls: list[dict[str, object]] = []
        outgoing_calls: list[dict[str, object]] = []
        if raw_items:
            incoming_raw = self.request(
                "callHierarchy/incomingCalls",
                {"item": raw_items[0]},
                timeout_s=30,
            )
            outgoing_raw = self.request(
                "callHierarchy/outgoingCalls",
                {"item": raw_items[0]},
                timeout_s=30,
            )
            incoming_calls = lsp_call_hierarchy_incoming_calls(
                os.environ["EOS_PLUGIN_WORKSPACE_ROOT"],
                incoming_raw,
            )
            outgoing_calls = lsp_call_hierarchy_outgoing_calls(
                os.environ["EOS_PLUGIN_WORKSPACE_ROOT"],
                outgoing_raw,
            )
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "items": items,
            "item_count": len(items),
            "item_names": [str(item.get("name", "")) for item in items],
            "incoming_calls": incoming_calls,
            "incoming_count": len(incoming_calls),
            "incoming_names": [
                str(call.get("from", {}).get("name", ""))
                for call in incoming_calls
                if isinstance(call.get("from"), dict)
            ],
            "outgoing_calls": outgoing_calls,
            "outgoing_count": len(outgoing_calls),
            "outgoing_names": [
                str(call.get("to", {}).get("name", ""))
                for call in outgoing_calls
                if isinstance(call.get("to"), dict)
            ],
        }

    def references(
        self,
        path: str,
        line: int,
        character: int,
        include_declaration: bool,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/references",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
                "context": {"includeDeclaration": include_declaration},
            },
            timeout_s=30,
        )
        locations = lsp_locations(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], raw)
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "position": {"line": line, "character": character},
            "include_declaration": include_declaration,
            "locations": locations,
            "reference_count": len(locations),
        }

    def rename(
        self,
        path: str,
        line: int,
        character: int,
        new_name: str,
    ) -> dict[str, object]:
        self.ensure_started()
        uri = self.open_or_change(path)
        raw = self.request(
            "textDocument/rename",
            {
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
                "newName": new_name,
            },
            timeout_s=30,
        )
        changes = workspace_edit_to_changes(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], raw)
        return {
            "protocol": "lsp-jsonrpc",
            "server": "pyright-langserver",
            "pid": self.proc.pid if self.proc is not None else None,
            "path": path,
            "new_name": new_name,
            "edit": raw,
            "changes": changes,
            "changed_paths": [str(change["path"]) for change in changes],
        }

    def open_or_change(self, path: str) -> str:
        target = os.path.join(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], path)
        with open(target, "r", encoding="utf-8") as handle:
            text = handle.read()
        uri = file_uri(target)
        version = self.opened_versions.get(uri, 0) + 1
        self.opened_versions[uri] = version
        if version == 1:
            self.notify(
                "textDocument/didOpen",
                {
                    "textDocument": {
                        "uri": uri,
                        "languageId": "python",
                        "version": version,
                        "text": text,
                    }
                },
            )
        else:
            self.notify(
                "textDocument/didChange",
                {
                    "textDocument": {"uri": uri, "version": version},
                    "contentChanges": [{"text": text}],
                },
            )
        return uri

    def request(self, method: str, params: dict[str, object], *, timeout_s: float) -> object:
        request_id = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + timeout_s
        while True:
            message = self.read_message(deadline)
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise RuntimeError(f"pyright {method} error: {message['error']}")
            return message.get("result")

    def wait_for_diagnostics(self, uri: str, *, timeout_s: float) -> list[dict[str, object]]:
        deadline = time.monotonic() + timeout_s
        latest: list[dict[str, object]] = []
        while time.monotonic() < deadline:
            message = self.read_message(deadline)
            if message.get("method") != "textDocument/publishDiagnostics":
                continue
            params = message.get("params")
            if not isinstance(params, dict) or params.get("uri") != uri:
                continue
            raw_diagnostics = params.get("diagnostics")
            latest = [
                diagnostic
                for diagnostic in raw_diagnostics
                if isinstance(diagnostic, dict)
            ] if isinstance(raw_diagnostics, list) else []
            if latest:
                return latest
        return latest

    def notify(self, method: str, params: dict[str, object]) -> None:
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def send(self, payload: dict[str, object]) -> None:
        if self.proc is None or self.proc.stdin is None:
            raise RuntimeError("pyright process is not running")
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        header = f"Content-Length: {len(body)}\r\n\r\n".encode("ascii")
        self.proc.stdin.write(header + body)
        self.proc.stdin.flush()

    def read_message(self, deadline: float) -> dict[str, object]:
        while b"\r\n\r\n" not in self.buffer:
            self.read_more(deadline)
        header, rest = self.buffer.split(b"\r\n\r\n", 1)
        content_length = 0
        for raw_line in header.split(b"\r\n"):
            name, _, value = raw_line.partition(b":")
            if name.lower() == b"content-length":
                content_length = int(value.strip())
        if content_length <= 0:
            raise RuntimeError(f"bad pyright LSP header: {header!r}")
        while len(rest) < content_length:
            self.read_more(deadline)
            header, rest = self.buffer.split(b"\r\n\r\n", 1)
        body = rest[:content_length]
        self.buffer = rest[content_length:]
        decoded = json.loads(body.decode("utf-8"))
        if not isinstance(decoded, dict):
            raise RuntimeError("pyright message was not an object")
        return decoded

    def read_more(self, deadline: float) -> None:
        if self.proc is None or self.proc.stdout is None:
            raise RuntimeError("pyright stdout unavailable")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("timed out waiting for pyright response")
        ready, _, _ = select.select([self.proc.stdout], [], [], remaining)
        if not ready:
            raise TimeoutError("timed out waiting for pyright response")
        chunk = os.read(self.proc.stdout.fileno(), 65536)
        if not chunk:
            stderr = ""
            if self.proc.stderr is not None:
                stderr = self.proc.stderr.read().decode("utf-8", "replace")
            raise RuntimeError(f"pyright closed stdout: {stderr[-1000:]}")
        self.buffer += chunk

    def close(self) -> None:
        proc = self.proc
        if proc is None:
            return
        try:
            self.notify("exit", {})
        except Exception:
            pass
        try:
            proc.terminate()
            proc.wait(timeout=1)
        except Exception:
            proc.kill()


def file_uri(path: str) -> str:
    from urllib.parse import quote

    return "file://" + quote(os.path.abspath(path))


def file_path_from_uri(uri: str) -> str:
    from urllib.parse import unquote, urlparse

    parsed = urlparse(uri)
    if parsed.scheme != "file":
        raise RuntimeError(f"unsupported workspace edit URI scheme: {uri!r}")
    return os.path.abspath(unquote(parsed.path))


def lsp_locations(workspace_root: str, raw: object) -> list[dict[str, object]]:
    raw_locations = raw if isinstance(raw, list) else [raw]
    root = os.path.abspath(workspace_root)
    locations: list[dict[str, object]] = []
    for raw_location in raw_locations:
        if not isinstance(raw_location, dict):
            continue
        uri = raw_location.get("uri") or raw_location.get("targetUri")
        range_payload = (
            raw_location.get("range")
            or raw_location.get("targetSelectionRange")
            or raw_location.get("targetRange")
        )
        if not isinstance(uri, str) or not isinstance(range_payload, dict):
            continue
        absolute_path = file_path_from_uri(uri)
        if os.path.commonpath([root, absolute_path]) != root:
            raise RuntimeError(f"pyright location escaped workspace root: {absolute_path}")
        locations.append(
            {
                "uri": uri,
                "path": os.path.relpath(absolute_path, root),
                "range": range_payload,
            }
        )
    return locations


def lsp_workspace_symbols(workspace_root: str, raw: object) -> list[dict[str, object]]:
    if not isinstance(raw, list):
        return []
    root = os.path.abspath(workspace_root)
    symbols: list[dict[str, object]] = []
    for item in raw:
        if not isinstance(item, dict):
            continue
        name = item.get("name")
        location = item.get("location")
        if not isinstance(name, str) or not isinstance(location, dict):
            continue
        uri = location.get("uri")
        range_payload = location.get("range")
        if not isinstance(uri, str) or not isinstance(range_payload, dict):
            continue
        absolute_path = file_path_from_uri(uri)
        if os.path.commonpath([root, absolute_path]) != root:
            raise RuntimeError(f"pyright workspace symbol escaped workspace root: {absolute_path}")
        symbols.append(
            {
                "name": name,
                "kind": item.get("kind"),
                "path": os.path.relpath(absolute_path, root),
                "range": range_payload,
            }
        )
    return symbols


def lsp_markup_text(raw: object) -> str:
    if isinstance(raw, str):
        return raw
    if isinstance(raw, dict):
        value = raw.get("value")
        if isinstance(value, str):
            return value
        return json.dumps(raw, sort_keys=True)
    if isinstance(raw, list):
        return "\n".join(lsp_markup_text(item) for item in raw)
    return ""


def workspace_edit_to_changes(
    workspace_root: str,
    edit: object,
) -> list[dict[str, object]]:
    if not isinstance(edit, dict):
        raise RuntimeError("LSP operation did not return a WorkspaceEdit object")
    edits_by_path: dict[str, list[dict[str, object]]] = {}
    raw_changes = edit.get("changes")
    if isinstance(raw_changes, dict):
        for uri, text_edits in raw_changes.items():
            if isinstance(uri, str) and isinstance(text_edits, list):
                path = file_path_from_uri(uri)
                edits_by_path.setdefault(path, []).extend(
                    text_edit for text_edit in text_edits if isinstance(text_edit, dict)
                )
    raw_document_changes = edit.get("documentChanges")
    if isinstance(raw_document_changes, list):
        for document_change in raw_document_changes:
            if not isinstance(document_change, dict):
                continue
            text_document = document_change.get("textDocument")
            text_edits = document_change.get("edits")
            if not isinstance(text_document, dict) or not isinstance(text_edits, list):
                continue
            uri = text_document.get("uri")
            if isinstance(uri, str):
                path = file_path_from_uri(uri)
                edits_by_path.setdefault(path, []).extend(
                    text_edit for text_edit in text_edits if isinstance(text_edit, dict)
                )
    if not edits_by_path:
        raise RuntimeError("LSP operation returned no text edits")

    root = os.path.abspath(workspace_root)
    changes: list[dict[str, object]] = []
    for absolute_path, text_edits in edits_by_path.items():
        if os.path.commonpath([root, absolute_path]) != root:
            raise RuntimeError(f"pyright edit escaped workspace root: {absolute_path}")
        rel_path = os.path.relpath(absolute_path, root)
        with open(absolute_path, "r", encoding="utf-8") as handle:
            content = handle.read()
        new_content = apply_text_edits(content, text_edits)
        changes.append(
            {
                "kind": "write",
                "path": rel_path,
                "content_utf8": new_content,
            }
        )
    return changes


def apply_text_edits(content: str, edits: list[dict[str, object]]) -> str:
    spans: list[tuple[int, int, str]] = []
    for edit in edits:
        range_payload = edit.get("range")
        if not isinstance(range_payload, dict):
            raise RuntimeError(f"pyright text edit missing range: {edit!r}")
        start = range_payload.get("start")
        end = range_payload.get("end")
        if not isinstance(start, dict) or not isinstance(end, dict):
            raise RuntimeError(f"pyright text edit has invalid range: {edit!r}")
        new_text = edit.get("newText")
        if not isinstance(new_text, str):
            raise RuntimeError(f"pyright text edit missing newText: {edit!r}")
        spans.append(
            (
                position_to_offset(content, start),
                position_to_offset(content, end),
                new_text,
            )
        )
    result = content
    for start, end, new_text in sorted(spans, key=lambda span: span[0], reverse=True):
        result = result[:start] + new_text + result[end:]
    return result


def position_to_offset(content: str, position: dict[str, object]) -> int:
    line = int(position.get("line", 0))
    character = int(position.get("character", 0))
    line_starts = [0]
    for index, char in enumerate(content):
        if char == "\n":
            line_starts.append(index + 1)
    if line >= len(line_starts):
        return len(content)
    line_start = line_starts[line]
    line_end = line_starts[line + 1] - 1 if line + 1 < len(line_starts) else len(content)
    return min(line_start + character, line_end)


def flatten_symbols(raw: object) -> list[dict[str, object]]:
    symbols: list[dict[str, object]] = []

    def visit(item: object) -> None:
        if not isinstance(item, dict):
            return
        name = item.get("name")
        if isinstance(name, str):
            symbols.append(
                {
                    "name": name,
                    "kind": item.get("kind"),
                    "range": item.get("range") or item.get("location", {}).get("range"),
                }
            )
        for child in item.get("children") or []:
            visit(child)

    if isinstance(raw, list):
        for entry in raw:
            visit(entry)
    return symbols


def lsp_completion_items(raw: object) -> list[dict[str, object]]:
    raw_items = lsp_completion_raw_items(raw)
    items: list[dict[str, object]] = []
    for item in raw_items:
        label = item.get("label")
        if not isinstance(label, str):
            continue
        insert_text = item.get("insertText")
        items.append(
            {
                "label": label,
                "kind": item.get("kind"),
                "detail": item.get("detail"),
                "insertText": insert_text if isinstance(insert_text, str) else None,
            }
        )
    return items


def lsp_completion_raw_items(raw: object) -> list[dict[str, object]]:
    if isinstance(raw, dict):
        raw_items = raw.get("items")
    else:
        raw_items = raw
    if not isinstance(raw_items, list):
        return []
    return [item for item in raw_items if isinstance(item, dict)]


def lsp_signature_items(raw: object) -> list[dict[str, object]]:
    if not isinstance(raw, dict):
        return []
    raw_signatures = raw.get("signatures")
    if not isinstance(raw_signatures, list):
        return []
    signatures: list[dict[str, object]] = []
    for signature in raw_signatures:
        if not isinstance(signature, dict):
            continue
        label = signature.get("label")
        if not isinstance(label, str):
            continue
        raw_parameters = signature.get("parameters")
        parameters = []
        if isinstance(raw_parameters, list):
            for parameter in raw_parameters:
                if not isinstance(parameter, dict):
                    continue
                parameters.append(
                    {
                        "label": parameter.get("label"),
                        "documentation": parameter.get("documentation"),
                    }
                )
        signatures.append(
            {
                "label": label,
                "documentation": signature.get("documentation"),
                "parameters": parameters,
            }
        )
    return signatures


def lsp_call_hierarchy_item(
    workspace_root: str,
    raw: object,
) -> dict[str, object] | None:
    if not isinstance(raw, dict):
        return None
    name = raw.get("name")
    uri = raw.get("uri")
    if not isinstance(name, str) or not isinstance(uri, str):
        return None
    root = os.path.abspath(workspace_root)
    absolute_path = file_path_from_uri(uri)
    if os.path.commonpath([root, absolute_path]) != root:
        raise RuntimeError(f"pyright call hierarchy escaped workspace root: {absolute_path}")
    return {
        "name": name,
        "kind": raw.get("kind"),
        "detail": raw.get("detail"),
        "uri": uri,
        "path": os.path.relpath(absolute_path, root),
        "range": raw.get("range"),
        "selectionRange": raw.get("selectionRange"),
    }


def lsp_call_hierarchy_items(
    workspace_root: str,
    raw: object,
) -> list[dict[str, object]]:
    raw_items = raw if isinstance(raw, list) else [raw]
    items: list[dict[str, object]] = []
    for raw_item in raw_items:
        item = lsp_call_hierarchy_item(workspace_root, raw_item)
        if item is not None:
            items.append(item)
    return items


def lsp_call_hierarchy_incoming_calls(
    workspace_root: str,
    raw: object,
) -> list[dict[str, object]]:
    if not isinstance(raw, list):
        return []
    calls: list[dict[str, object]] = []
    for raw_call in raw:
        if not isinstance(raw_call, dict):
            continue
        source = lsp_call_hierarchy_item(workspace_root, raw_call.get("from"))
        if source is None:
            continue
        raw_ranges = raw_call.get("fromRanges")
        calls.append(
            {
                "from": source,
                "fromRanges": raw_ranges if isinstance(raw_ranges, list) else [],
            }
        )
    return calls


def lsp_call_hierarchy_outgoing_calls(
    workspace_root: str,
    raw: object,
) -> list[dict[str, object]]:
    if not isinstance(raw, list):
        return []
    calls: list[dict[str, object]] = []
    for raw_call in raw:
        if not isinstance(raw_call, dict):
            continue
        target = lsp_call_hierarchy_item(workspace_root, raw_call.get("to"))
        if target is None:
            continue
        raw_ranges = raw_call.get("fromRanges")
        calls.append(
            {
                "to": target,
                "fromRanges": raw_ranges if isinstance(raw_ranges, list) else [],
            }
        )
    return calls


def maybe_start_adapter() -> None:
    global ADAPTER
    if os.environ.get("EOS_PLUGIN_SERVICE_ID") != "adapter_harness":
        return
    ADAPTER = PackageAdapter()
    initial = ADAPTER.load(ADAPTER_WATCH_PATH)
    log("adapter_started", initial=initial)


def maybe_refresh_adapter() -> None:
    if ADAPTER is None:
        return
    refreshed = ADAPTER.load(ADAPTER_WATCH_PATH)
    log("adapter_refreshed", refreshed=refreshed)


def maybe_refresh_pyright() -> None:
    if PYRIGHT is None:
        return
    PYRIGHT.refresh()


def handle_request(sock: socket.socket, request: dict[str, object]) -> None:
    global CURRENT_MANIFEST_KEY, PYRIGHT

    if request["direction"] != "request":
        raise RuntimeError(f"expected request direction, got {request['direction']!r}")
    op = str(request["op"])
    body = request["body"]
    if not isinstance(body, dict):
        raise RuntimeError(f"expected object body for {op}, got {type(body).__name__}")
    log("request", op=op, message_id=request["message_id"], body=body)

    if op == "daemon.workspace_snapshot_refresh":
        message_type = str(body.get("type") or "")
        target_manifest = body.get("target_manifest_key") or body.get("manifest_key")
        if target_manifest:
            CURRENT_MANIFEST_KEY = str(target_manifest)
        if (
            os.environ.get("EOS_PLUGIN_SERVICE_ID") == "health_fail_harness"
            and message_type == "health"
        ):
            reply(
                sock,
                request,
                {
                    "manifest_key": CURRENT_MANIFEST_KEY,
                    "accepted": False,
                    "reason": "intentional health failure",
                },
            )
            return
        if not CURRENT_MANIFEST_KEY:
            reply(sock, request, {"manifest_key": "", "accepted": False, "reason": "missing manifest"})
            return
        if message_type == "swap_workspace":
            maybe_refresh_adapter()
            maybe_refresh_pyright()
        reply(
            sock,
            request,
            {
                "manifest_key": CURRENT_MANIFEST_KEY,
                "accepted": True,
                "refresh_type": message_type,
            },
        )
        return

    if op == "plugin.generic.pyright_symbols":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_pyright.py")
        query = str(body.get("query") or "")
        try:
            lsp_reply = PYRIGHT.document_symbols(read_path, query)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_workspace_symbols":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        query = str(body.get("query") or "")
        try:
            lsp_reply = PYRIGHT.workspace_symbols(query)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_capabilities":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        try:
            lsp_reply = PYRIGHT.capabilities()
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_document_formatting":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_pyright.py")
        try:
            lsp_reply = PYRIGHT.document_formatting(read_path)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": not bool(lsp_reply.get("unsupported")),
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_execute_command":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        command = str(body.get("command") or "")
        try:
            lsp_reply = PYRIGHT.execute_command(command)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": not bool(lsp_reply.get("unsupported")),
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_completion":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_completion.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        query = str(body.get("query") or "")
        try:
            lsp_reply = PYRIGHT.completion(read_path, line, character, query)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_completion_resolve":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_completion.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        query = str(body.get("query") or "")
        try:
            lsp_reply = PYRIGHT.completion_resolve(
                read_path, line, character, query
            )
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_diagnostics":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_diagnostics.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        query = str(body.get("query") or "")
        try:
            lsp_reply = PYRIGHT.diagnostics(read_path, line, character, query)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_code_actions":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_diagnostics.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        raw_only = body.get("only")
        only = [str(item) for item in raw_only] if isinstance(raw_only, list) else []
        raw_diagnostics = body.get("diagnostics")
        diagnostics = [
            item
            for item in raw_diagnostics
            if isinstance(item, dict)
        ] if isinstance(raw_diagnostics, list) else []
        try:
            lsp_reply = PYRIGHT.code_actions(
                read_path,
                line,
                character,
                only,
                diagnostics,
            )
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_signature_help":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_signature.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        try:
            lsp_reply = PYRIGHT.signature_help(read_path, line, character)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_definition":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_pyright.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        try:
            lsp_reply = PYRIGHT.definition(read_path, line, character)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_hover":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_pyright.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        try:
            lsp_reply = PYRIGHT.hover(read_path, line, character)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_type_definition":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_type.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        try:
            lsp_reply = PYRIGHT.type_definition(read_path, line, character)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_declaration":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_pyright.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        try:
            lsp_reply = PYRIGHT.declaration(read_path, line, character)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_call_hierarchy":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_call_hierarchy.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        try:
            lsp_reply = PYRIGHT.call_hierarchy(read_path, line, character)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_document_highlight":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_pyright.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        try:
            lsp_reply = PYRIGHT.document_highlight(read_path, line, character)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_prepare_rename":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_pyright.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        try:
            lsp_reply = PYRIGHT.prepare_rename(read_path, line, character)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_references":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_pyright.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        include_declaration = bool(body.get("include_declaration", True))
        try:
            lsp_reply = PYRIGHT.references(
                read_path,
                line,
                character,
                include_declaration,
            )
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_pyright_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
            },
        )
        return

    if op == "plugin.generic.pyright_rename":
        if PYRIGHT is None:
            PYRIGHT = PyrightAdapter()
        read_path = str(body.get("read_path") or "live_plugin_pyright.py")
        line = int(body.get("line") or 0)
        character = int(body.get("character") or 0)
        new_name = str(body.get("new_name") or "")
        if not new_name:
            reply(sock, request, {"success": False, "error": "missing new_name"})
            return
        try:
            lsp_reply = PYRIGHT.rename(read_path, line, character, new_name)
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_pyright_adapter": True,
                    "from_self_managed": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        callback_id = f"{request['message_id']}:occ"
        write_frame(
            sock,
            op="daemon.occ.apply_changeset",
            message_id=callback_id,
            direction="request",
            body={
                "layer_stack_root": os.environ["EOS_PLUGIN_LAYER_STACK_ROOT"],
                "changes": lsp_reply["changes"],
            },
        )
        callback_reply = read_frame(sock)
        if callback_reply["direction"] != "reply":
            raise RuntimeError("Pyright OCC callback did not return a reply frame")
        if callback_reply["message_id"] != callback_id:
            raise RuntimeError("Pyright OCC callback reply message_id mismatch")
        callback_body = callback_reply["body"]
        if not isinstance(callback_body, dict):
            raise RuntimeError("Pyright OCC callback reply body was not an object")
        reply(
            sock,
            request,
            {
                "success": bool(callback_body.get("success")),
                "from_ppc": True,
                "from_pyright_adapter": True,
                "from_self_managed": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "lsp": lsp_reply,
                "changed_paths": lsp_reply["changed_paths"],
                "callback": callback_body,
            },
        )
        return

    if op == "plugin.generic.lsp_apply_workspace_edit":
        raw_edit = body.get("edit")
        try:
            changes = workspace_edit_to_changes(
                os.environ["EOS_PLUGIN_WORKSPACE_ROOT"],
                raw_edit,
            )
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_lsp_workspace_edit": True,
                    "from_self_managed": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        callback_id = f"{request['message_id']}:occ"
        write_frame(
            sock,
            op="daemon.occ.apply_changeset",
            message_id=callback_id,
            direction="request",
            body={
                "layer_stack_root": os.environ["EOS_PLUGIN_LAYER_STACK_ROOT"],
                "changes": changes,
            },
        )
        callback_reply = read_frame(sock)
        if callback_reply["direction"] != "reply":
            raise RuntimeError(
                "LSP apply WorkspaceEdit OCC callback did not return a reply frame"
            )
        if callback_reply["message_id"] != callback_id:
            raise RuntimeError(
                "LSP apply WorkspaceEdit callback reply message_id mismatch"
            )
        callback_body = callback_reply["body"]
        if not isinstance(callback_body, dict):
            raise RuntimeError("LSP apply WorkspaceEdit callback reply body was not an object")
        reply(
            sock,
            request,
            {
                "success": bool(callback_body.get("success")),
                "from_ppc": True,
                "from_lsp_workspace_edit": True,
                "from_self_managed": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "edit": raw_edit if isinstance(raw_edit, dict) else {},
                "changed_paths": [str(change["path"]) for change in changes],
                "changes": changes,
                "callback": callback_body,
            },
        )
        return

    if op == "plugin.generic.lsp_apply_code_action":
        raw_action = body.get("action")
        raw_edit = raw_action.get("edit") if isinstance(raw_action, dict) else None
        try:
            changes = workspace_edit_to_changes(
                os.environ["EOS_PLUGIN_WORKSPACE_ROOT"],
                raw_edit,
            )
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_lsp_code_action": True,
                    "from_self_managed": True,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        callback_id = f"{request['message_id']}:occ"
        write_frame(
            sock,
            op="daemon.occ.apply_changeset",
            message_id=callback_id,
            direction="request",
            body={
                "layer_stack_root": os.environ["EOS_PLUGIN_LAYER_STACK_ROOT"],
                "changes": changes,
            },
        )
        callback_reply = read_frame(sock)
        if callback_reply["direction"] != "reply":
            raise RuntimeError(
                "LSP apply CodeAction OCC callback did not return a reply frame"
            )
        if callback_reply["message_id"] != callback_id:
            raise RuntimeError("LSP apply CodeAction callback reply message_id mismatch")
        callback_body = callback_reply["body"]
        if not isinstance(callback_body, dict):
            raise RuntimeError("LSP apply CodeAction callback reply body was not an object")
        action = raw_action if isinstance(raw_action, dict) else {}
        reply(
            sock,
            request,
            {
                "success": bool(callback_body.get("success")),
                "from_ppc": True,
                "from_lsp_code_action": True,
                "from_self_managed": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "action_title": str(action.get("title", "")),
                "action_kind": str(action.get("kind", "")),
                "action": action,
                "changed_paths": [str(change["path"]) for change in changes],
                "changes": changes,
                "callback": callback_body,
            },
        )
        return

    if op == "plugin.generic.lsp_format_document":
        path = str(body.get("path") or "")
        raw_edits = body.get("edits")
        if not path:
            reply(sock, request, {"success": False, "error": "missing path"})
            return
        target_uri = file_uri(os.path.join(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], path))
        raw_edit = {
            "changes": {
                target_uri: raw_edits if isinstance(raw_edits, list) else [],
            }
        }
        try:
            changes = workspace_edit_to_changes(
                os.environ["EOS_PLUGIN_WORKSPACE_ROOT"],
                raw_edit,
            )
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_lsp_formatting": True,
                    "from_self_managed": True,
                    "method": "textDocument/formatting",
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        callback_body = send_occ_callback(
            sock,
            request,
            suffix="occ",
            changes=changes,
        )
        reply(
            sock,
            request,
            {
                "success": bool(callback_body.get("success")),
                "from_ppc": True,
                "from_lsp_formatting": True,
                "from_self_managed": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "protocol": "lsp-jsonrpc",
                "method": "textDocument/formatting",
                "path": path,
                "edits": raw_edits if isinstance(raw_edits, list) else [],
                "edit_count": len(raw_edits) if isinstance(raw_edits, list) else 0,
                "changed_paths": [str(change["path"]) for change in changes],
                "changes": changes,
                "callback": callback_body,
            },
        )
        return

    if op == "plugin.generic.lsp_execute_command":
        command = str(body.get("command") or "")
        commands = ["generic.applyWorkspaceEdit"]
        raw_arguments = body.get("arguments")
        first_argument = (
            raw_arguments[0]
            if isinstance(raw_arguments, list)
            and raw_arguments
            and isinstance(raw_arguments[0], dict)
            else {}
        )
        raw_edit = (
            first_argument.get("edit")
            if isinstance(first_argument, dict)
            else None
        )
        if command not in commands:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_lsp_execute_command": True,
                    "from_self_managed": True,
                    "protocol": "lsp-jsonrpc",
                    "method": "workspace/executeCommand",
                    "command": command,
                    "commands": commands,
                    "supported": False,
                    "unsupported": True,
                    "error": "unsupported command",
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        try:
            changes = workspace_edit_to_changes(
                os.environ["EOS_PLUGIN_WORKSPACE_ROOT"],
                raw_edit,
            )
        except Exception as exc:
            reply(
                sock,
                request,
                {
                    "success": False,
                    "from_ppc": True,
                    "from_lsp_execute_command": True,
                    "from_self_managed": True,
                    "protocol": "lsp-jsonrpc",
                    "method": "workspace/executeCommand",
                    "command": command,
                    "commands": commands,
                    "supported": True,
                    "unsupported": False,
                    "error": str(exc),
                    "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                    "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                    "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                    "manifest_key": CURRENT_MANIFEST_KEY,
                },
            )
            return
        callback_body = send_occ_callback(
            sock,
            request,
            suffix="occ",
            changes=changes,
        )
        changed_paths = [str(change["path"]) for change in changes]
        reply(
            sock,
            request,
            {
                "success": bool(callback_body.get("success")),
                "from_ppc": True,
                "from_lsp_execute_command": True,
                "from_self_managed": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "protocol": "lsp-jsonrpc",
                "method": "workspace/executeCommand",
                "command": command,
                "commands": commands,
                "supported": True,
                "unsupported": False,
                "arguments": raw_arguments if isinstance(raw_arguments, list) else [],
                "changed_paths": changed_paths,
                "changes": changes,
                "callback": callback_body,
                "result": {
                    "applied": bool(callback_body.get("success")),
                    "changed_paths": changed_paths,
                },
            },
        )
        return

    if op == "plugin.generic.adapter_query":
        if ADAPTER is None:
            reply(sock, request, {"success": False, "error": "adapter was not started"})
            return
        read_path = str(body.get("read_path") or ADAPTER_WATCH_PATH)
        package_reply = ADAPTER.query(read_path)
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_package_adapter": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
                "package": package_reply,
            },
        )
        return

    if op == "plugin.generic.crash_probe":
        log("intentional_crash", message_id=request["message_id"])
        os._exit(7)

    if op == "plugin.generic.hang_probe":
        seconds = float(body.get("sleep_s") or 10.0)
        log("intentional_hang", message_id=request["message_id"], sleep_s=seconds)
        time.sleep(seconds)
        reply(sock, request, {"success": False, "error": "unexpected hang completion"})
        return

    if op == "plugin.generic.recover_probe":
        marker = "/eos/plugin/recover_probe_once.flag"
        if not os.path.exists(marker):
            with open(marker, "w", encoding="utf-8") as handle:
                handle.write("crashed-once\n")
            log("recover_probe_exit_once", message_id=request["message_id"])
            os._exit(9)
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_recovered_service": True,
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "manifest_key": CURRENT_MANIFEST_KEY,
            },
        )
        return

    if op in {
        "plugin.generic.ping",
        "plugin.generic.restart_ping",
        "plugin.generic.crash_recover_ping",
        "plugin.generic.health_fail_recover_ping",
        "plugin.generic.hang_recover_ping",
    }:
        workspace_read: dict[str, object] = {"requested": False}
        read_path = body.get("read_path")
        if read_path:
            target = os.path.join(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"], str(read_path))
            try:
                with open(target, "r", encoding="utf-8") as handle:
                    workspace_read = {
                        "requested": True,
                        "exists": True,
                        "path": str(read_path),
                        "content": handle.read(),
                    }
            except FileNotFoundError:
                workspace_read = {
                    "requested": True,
                    "exists": False,
                    "path": str(read_path),
                }
        reply(
            sock,
            request,
            {
                "success": True,
                "from_ppc": True,
                "from_restart_service": op == "plugin.generic.restart_ping",
                "from_crash_recovered_service": op
                == "plugin.generic.crash_recover_ping",
                "from_health_recovered_service": op
                == "plugin.generic.health_fail_recover_ping",
                "from_timeout_recovered_service": op
                == "plugin.generic.hang_recover_ping",
                "plugin_id": os.environ.get("EOS_PLUGIN_ID"),
                "service_id": os.environ.get("EOS_PLUGIN_SERVICE_ID"),
                "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
                "workspace_read": workspace_read,
                "manifest_key": CURRENT_MANIFEST_KEY,
                "echo": body.get("message"),
            },
        )
        return

    if op == "plugin.generic.apply":
        path = str(body.get("path") or "")
        content = str(body.get("content") or "")
        if not path:
            reply(sock, request, {"success": False, "error": "missing path"})
            return
        callback_body = send_occ_callback(
            sock,
            request,
            suffix="occ",
            changes=[
                {
                    "kind": "write",
                    "path": path,
                    "content_utf8": content,
                }
            ],
        )
        reply(
            sock,
            request,
            {
                "success": bool(callback_body.get("success")),
                "from_self_managed": True,
                "callback": callback_body,
            },
        )
        return

    if op == "plugin.generic.apply_multi":
        writes = body.get("writes")
        if not isinstance(writes, list) or not writes:
            reply(sock, request, {"success": False, "error": "missing writes"})
            return
        callbacks: list[dict[str, object]] = []
        changed_paths: list[str] = []
        for index, write in enumerate(writes):
            if not isinstance(write, dict):
                reply(sock, request, {"success": False, "error": "invalid write"})
                return
            path = str(write.get("path") or "")
            content = str(write.get("content") or "")
            if not path:
                reply(sock, request, {"success": False, "error": "missing path"})
                return
            callback_body = send_occ_callback(
                sock,
                request,
                suffix=f"occ:{index}",
                changes=[
                    {
                        "kind": "write",
                        "path": path,
                        "content_utf8": content,
                    }
                ],
            )
            callbacks.append(
                {
                    "index": index,
                    "path": path,
                    "success": bool(callback_body.get("success")),
                    "callback": callback_body,
                }
            )
            changed_paths.append(path)
        reply(
            sock,
            request,
            {
                "success": all(callback["success"] for callback in callbacks),
                "from_self_managed": True,
                "callback_count": len(callbacks),
                "callbacks": callbacks,
                "changed_paths": changed_paths,
            },
        )
        return

    reply(sock, request, {"success": False, "error": f"unknown op {op}"})


def main() -> int:
    socket_path = os.environ["EOS_PLUGIN_PPC_SOCKET"]
    try:
        maybe_start_adapter()
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(socket_path)
        log(
            "connected",
            socket_path=socket_path,
            layer_stack_root=os.environ.get("EOS_PLUGIN_LAYER_STACK_ROOT"),
            workspace_root=os.environ.get("EOS_PLUGIN_WORKSPACE_ROOT"),
        )
        while True:
            handle_request(sock, read_frame(sock))
    except EOFError:
        log("closed")
        return 0
    except Exception as exc:
        log("error", error=str(exc), traceback=traceback.format_exc())
        return 1
    finally:
        if ADAPTER is not None:
            ADAPTER.close()
        if PYRIGHT is not None:
            PYRIGHT.close()


if __name__ == "__main__":
    raise SystemExit(main())
'''

VANILLA_PACKAGE_SOURCE = r'''
from __future__ import annotations

import json
import os
import sys
from pathlib import Path

CACHE: dict[str, dict[str, object]] = {}


def load(workspace_root: str, path: str) -> dict[str, object]:
    target = Path(workspace_root) / path
    try:
        content = target.read_text(encoding="utf-8")
        entry: dict[str, object] = {
            "exists": True,
            "path": path,
            "content": content,
            "pid": os.getpid(),
            "protocol": "line-json-v1",
        }
    except FileNotFoundError:
        entry = {
            "exists": False,
            "path": path,
            "pid": os.getpid(),
            "protocol": "line-json-v1",
        }
    CACHE[path] = entry
    return entry


def query(path: str) -> dict[str, object]:
    entry = CACHE.get(path)
    if entry is None:
        return {
            "exists": False,
            "path": path,
            "cached": False,
            "pid": os.getpid(),
            "protocol": "line-json-v1",
        }
    result = dict(entry)
    result["cached"] = True
    return result


def main() -> int:
    for line in sys.stdin:
        request = json.loads(line)
        op = request.get("op")
        if op == "load":
            response = load(str(request["workspace_root"]), str(request["path"]))
        elif op == "query":
            response = query(str(request["path"]))
        elif op == "shutdown":
            print(json.dumps({"success": True, "shutdown": True}), flush=True)
            return 0
        else:
            response = {"success": False, "error": f"unknown op {op}"}
        print(json.dumps(response, sort_keys=True), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
'''

RUNTIME_BRIDGE_SERVER_SOURCE = r'''
from __future__ import annotations

import asyncio
import os
import time
from pathlib import Path
from typing import Any

from sandbox.ephemeral_workspace.plugin.op_registry import register_plugin_op
from sandbox.shared.models import Intent


def _workspace_read(workspace_root: str, rel_path: str) -> dict[str, object]:
    if not rel_path:
        return {"requested": False}
    target = Path(workspace_root) / rel_path
    try:
        return {
            "requested": True,
            "exists": True,
            "path": rel_path,
            "content": target.read_text(encoding="utf-8"),
        }
    except FileNotFoundError:
        return {"requested": True, "exists": False, "path": rel_path}


def _flatten_lsp_symbols(symbols: Any) -> list[dict[str, Any]]:
    flat: list[dict[str, Any]] = []
    if not isinstance(symbols, list):
        return flat
    for symbol in symbols:
        if not isinstance(symbol, dict):
            continue
        flat.append(symbol)
        flat.extend(_flatten_lsp_symbols(symbol.get("children")))
    return flat


def _lsp_hover_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        raw = value.get("value")
        if isinstance(raw, str):
            return raw
        raw = value.get("contents")
        if raw is not value:
            return _lsp_hover_text(raw)
        return ""
    if isinstance(value, list):
        return "\n".join(
            text for item in value if (text := _lsp_hover_text(item))
        )
    return ""


def _lsp_location_path(workspace_root: str, location: Any) -> str:
    if not isinstance(location, dict):
        return ""
    raw_path = location.get("path") or location.get("file_path")
    if not isinstance(raw_path, str) or not raw_path:
        return ""
    path = Path(raw_path)
    if not path.is_absolute():
        return path.as_posix()
    try:
        return path.relative_to(Path(workspace_root)).as_posix()
    except ValueError:
        return path.name


def _lsp_location_paths(workspace_root: str, locations: Any) -> list[str]:
    if not isinstance(locations, list):
        return []
    return [
        path
        for location in locations
        if (path := _lsp_location_path(workspace_root, location))
    ]


def _lsp_location_start_lines(locations: Any) -> list[int]:
    if not isinstance(locations, list):
        return []
    lines: list[int] = []
    for location in locations:
        if not isinstance(location, dict):
            continue
        range_value = location.get("range")
        if not isinstance(range_value, dict):
            continue
        start = range_value.get("start")
        if isinstance(start, dict) and isinstance(start.get("line"), int):
            lines.append(start["line"])
    return lines


def _lsp_range_start_lines(items: Any) -> list[int]:
    if not isinstance(items, list):
        return []
    lines: list[int] = []
    for item in items:
        if not isinstance(item, dict):
            continue
        range_value = item.get("range")
        if not isinstance(range_value, dict):
            continue
        start = range_value.get("start")
        if isinstance(start, dict) and isinstance(start.get("line"), int):
            lines.append(start["line"])
    return lines


def _lsp_diagnostic_messages(diagnostics: Any) -> list[str]:
    if not isinstance(diagnostics, list):
        return []
    return [
        str(diagnostic.get("message") or "")
        for diagnostic in diagnostics
        if isinstance(diagnostic, dict)
    ]


@register_plugin_op("generic", "runtime_bridge_ping", intent=Intent.READ_ONLY)
def runtime_bridge_ping(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    workspace_root = str(ctx.overlay.workspace_root)
    return {
        "success": True,
        "from_runtime_bridge": True,
        "from_ppc_service_bridge": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "workspace_root": workspace_root,
        "manifest_key": ctx.overlay.active_manifest_key(),
        "projection_manifest_key": ctx.projection.active_manifest_key(),
        "workspace_read": _workspace_read(workspace_root, str(args.get("read_path") or "")),
    }


@register_plugin_op("generic", "lsp_bridge_find_definitions", intent=Intent.READ_ONLY)
async def lsp_bridge_find_definitions(
    args: dict[str, Any],
    ctx: Any,
) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    file_path = str(args.get("file_path") or args.get("read_path") or "")
    line = int(args.get("line") or 0)
    character = int(args.get("character") or 0)
    workspace_root = str(ctx.overlay.workspace_root)
    result = await lsp_server.find_definitions(
        {
            "file_path": file_path,
            "line": line,
            "character": character,
        },
        ctx,
    )
    definitions = result.get("definitions") if isinstance(result, dict) else []
    definitions = definitions if isinstance(definitions, list) else []
    return {
        "success": True,
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "path": file_path,
            "position": {"line": line, "character": character},
            "definition_count": len(definitions),
            "definition_paths": _lsp_location_paths(workspace_root, definitions),
            "definition_start_lines": _lsp_location_start_lines(definitions),
            "definitions": definitions,
            "raw": result,
        },
    }


@register_plugin_op("generic", "lsp_bridge_find_references", intent=Intent.READ_ONLY)
async def lsp_bridge_find_references(
    args: dict[str, Any],
    ctx: Any,
) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    file_path = str(args.get("file_path") or args.get("read_path") or "")
    line = int(args.get("line") or 0)
    character = int(args.get("character") or 0)
    include_declaration = bool(args.get("include_declaration", True))
    workspace_root = str(ctx.overlay.workspace_root)
    result = await lsp_server.find_references(
        {
            "file_path": file_path,
            "line": line,
            "character": character,
            "include_declaration": include_declaration,
        },
        ctx,
    )
    references = result.get("references") if isinstance(result, dict) else []
    references = references if isinstance(references, list) else []
    return {
        "success": True,
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "path": file_path,
            "position": {"line": line, "character": character},
            "include_declaration": include_declaration,
            "reference_count": len(references),
            "reference_paths": _lsp_location_paths(workspace_root, references),
            "reference_start_lines": _lsp_location_start_lines(references),
            "references": references,
            "raw": result,
        },
    }


@register_plugin_op("generic", "lsp_bridge_signature_help", intent=Intent.READ_ONLY)
async def lsp_bridge_signature_help(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    file_path = str(args.get("file_path") or args.get("read_path") or "")
    line = int(args.get("line") or 0)
    character = int(args.get("character") or 0)
    result = await lsp_server.signature_help(
        {
            "file_path": file_path,
            "line": line,
            "character": character,
        },
        ctx,
    )
    signatures = result.get("signatures") if isinstance(result, dict) else []
    signatures = signatures if isinstance(signatures, list) else []
    labels = result.get("labels") if isinstance(result, dict) else []
    labels = labels if isinstance(labels, list) else []
    return {
        "success": True,
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "path": file_path,
            "position": {"line": line, "character": character},
            "signature_count": len(signatures),
            "labels": [str(label) for label in labels],
            "active_signature": (
                result.get("active_signature") if isinstance(result, dict) else None
            ),
            "active_parameter": (
                result.get("active_parameter") if isinstance(result, dict) else None
            ),
            "signatures": signatures,
            "raw": result,
        },
    }


@register_plugin_op("generic", "lsp_bridge_document_highlight", intent=Intent.READ_ONLY)
async def lsp_bridge_document_highlight(
    args: dict[str, Any],
    ctx: Any,
) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    file_path = str(args.get("file_path") or args.get("read_path") or "")
    line = int(args.get("line") or 0)
    character = int(args.get("character") or 0)
    result = await lsp_server.document_highlight(
        {
            "file_path": file_path,
            "line": line,
            "character": character,
        },
        ctx,
    )
    highlights = result.get("highlights") if isinstance(result, dict) else []
    highlights = highlights if isinstance(highlights, list) else []
    return {
        "success": True,
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "path": file_path,
            "position": {"line": line, "character": character},
            "highlight_count": len(highlights),
            "highlight_start_lines": _lsp_range_start_lines(highlights),
            "highlights": highlights,
            "raw": result,
        },
    }


@register_plugin_op("generic", "lsp_bridge_diagnostics", intent=Intent.READ_ONLY)
async def lsp_bridge_diagnostics(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    file_path = str(args.get("file_path") or args.get("read_path") or "")
    line = int(args.get("line") or 0)
    character = int(args.get("character") or 0)
    wait_for_diagnostics = bool(args.get("wait_for_diagnostics", False))
    result = await lsp_server.diagnostics(
        {
            "file_path": file_path,
            "line": line,
            "character": character,
            "wait_for_diagnostics": wait_for_diagnostics,
        },
        ctx,
    )
    diagnostics = result.get("diagnostics") if isinstance(result, dict) else []
    diagnostics = diagnostics if isinstance(diagnostics, list) else []
    return {
        "success": True,
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "path": file_path,
            "position": {"line": line, "character": character},
            "wait_for_diagnostics": wait_for_diagnostics,
            "diagnostic_count": len(diagnostics),
            "diagnostic_messages": _lsp_diagnostic_messages(diagnostics),
            "diagnostics": diagnostics,
            "raw": result,
        },
    }


@register_plugin_op("generic", "lsp_bridge_code_actions", intent=Intent.READ_ONLY)
async def lsp_bridge_code_actions(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    file_path = str(args.get("file_path") or args.get("read_path") or "")
    line = int(args.get("line") or 0)
    character = int(args.get("character") or 0)
    only = args.get("only") if isinstance(args.get("only"), list) else []
    request_args: dict[str, Any] = {
        "file_path": file_path,
        "line": line,
        "character": character,
    }
    if only:
        request_args["only"] = only
    if isinstance(args.get("range"), dict):
        request_args["range"] = args["range"]
    if isinstance(args.get("diagnostics"), list):
        request_args["diagnostics"] = args["diagnostics"]
    result = await lsp_server.code_actions(request_args, ctx)
    actions = result.get("code_actions") if isinstance(result, dict) else []
    actions = actions if isinstance(actions, list) else []
    return {
        "success": True,
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "path": file_path,
            "position": {"line": line, "character": character},
            "only": only,
            "action_count": len(actions),
            "action_titles": [
                str(action.get("title") or "")
                for action in actions
                if isinstance(action, dict)
            ],
            "action_kinds": [
                str(action.get("kind") or "")
                for action in actions
                if isinstance(action, dict)
            ],
            "actions": actions,
            "raw": result,
        },
    }


@register_plugin_op("generic", "lsp_bridge_hover", intent=Intent.READ_ONLY)
async def lsp_bridge_hover(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    file_path = str(args.get("file_path") or args.get("read_path") or "")
    line = int(args.get("line") or 0)
    character = int(args.get("character") or 0)
    result = await lsp_server.hover(
        {
            "file_path": file_path,
            "line": line,
            "character": character,
        },
        ctx,
    )
    hover = result.get("hover") if isinstance(result, dict) else {}
    return {
        "success": True,
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "path": file_path,
            "position": {"line": line, "character": character},
            "hover_text": _lsp_hover_text(hover),
            "raw": result,
        },
    }


@register_plugin_op("generic", "lsp_bridge_query_symbols", intent=Intent.READ_ONLY)
async def lsp_bridge_query_symbols(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    file_path = str(args.get("file_path") or args.get("read_path") or "")
    query = str(args.get("query") or "")
    query_args = {"file_path": file_path, "query": query}
    retry_after_timeout = False
    try:
        result = await lsp_server.query_symbols(query_args, ctx)
    except TimeoutError:
        retry_after_timeout = True
        await asyncio.sleep(0.25)
        result = await lsp_server.query_symbols(query_args, ctx)
    symbols = _flatten_lsp_symbols(result.get("symbols"))
    return {
        "success": True,
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "retry_after_timeout": retry_after_timeout,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "path": file_path,
            "query": query,
            "symbol_count": len(symbols),
            "symbol_names": [str(symbol.get("name", "")) for symbol in symbols],
            "raw": result,
        },
    }


@register_plugin_op(
    "generic",
    "lsp_bridge_rename",
    intent=Intent.WRITE_ALLOWED,
    auto_workspace_overlay=False,
)
async def lsp_bridge_rename(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    file_path = str(args.get("file_path") or args.get("read_path") or "")
    line = int(args.get("line") or 0)
    character = int(args.get("character") or 0)
    new_name = str(args.get("new_name") or "")
    result = await lsp_server.rename(
        {
            "file_path": file_path,
            "line": line,
            "character": character,
            "new_name": new_name,
        },
        ctx,
    )
    apply_result = result.get("apply") if isinstance(result, dict) else {}
    apply_result = apply_result if isinstance(apply_result, dict) else {}
    changed_paths = [
        str(path)
        for path in apply_result.get("changed_paths", [])
        if isinstance(path, str)
    ]
    return {
        "success": bool(apply_result.get("success")),
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "from_mounted_workspace_callback": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "changed_paths": changed_paths,
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "path": file_path,
            "position": {"line": line, "character": character},
            "new_name": new_name,
            "edit": result.get("edit") if isinstance(result, dict) else {},
            "apply": apply_result,
        },
    }


@register_plugin_op(
    "generic",
    "lsp_bridge_apply_workspace_edit",
    intent=Intent.WRITE_ALLOWED,
    auto_workspace_overlay=False,
)
async def lsp_bridge_apply_workspace_edit(
    args: dict[str, Any],
    ctx: Any,
) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    result = await lsp_server.apply_workspace_edit_op(args, ctx)
    changed_paths = [
        str(path) for path in result.get("changed_paths", []) if isinstance(path, str)
    ]
    return {
        "success": bool(result.get("success")),
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "from_mounted_workspace_callback": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "changed_paths": changed_paths,
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "apply": result,
        },
    }


@register_plugin_op(
    "generic",
    "lsp_bridge_apply_code_action",
    intent=Intent.WRITE_ALLOWED,
    auto_workspace_overlay=False,
)
async def lsp_bridge_apply_code_action(
    args: dict[str, Any],
    ctx: Any,
) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime import server as lsp_server

    result = await lsp_server.apply_code_action(args, ctx)
    action = result.get("action") if isinstance(result, dict) else {}
    action = action if isinstance(action, dict) else {}
    apply_result = result.get("apply") if isinstance(result, dict) else {}
    apply_result = apply_result if isinstance(apply_result, dict) else {}
    changed_paths = [
        str(path)
        for path in apply_result.get("changed_paths", [])
        if isinstance(path, str)
    ]
    return {
        "success": bool(apply_result.get("success")),
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "from_mounted_workspace_callback": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "changed_paths": changed_paths,
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.server",
            "action_title": str(action.get("title") or ""),
            "action_kind": str(action.get("kind") or ""),
            "action": action,
            "apply": apply_result,
        },
    }


@register_plugin_op(
    "generic",
    "lsp_bridge_format_document",
    intent=Intent.WRITE_ALLOWED,
    auto_workspace_overlay=False,
)
async def lsp_bridge_format_document(
    args: dict[str, Any],
    ctx: Any,
) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime.apply import apply_workspace_edit

    path = str(args.get("path") or "")
    raw_edits = args.get("edits")
    if not path:
        return {"success": False, "error": "missing path"}
    workspace_root = str(ctx.overlay.workspace_root)
    edit = {
        "changes": {
            f"file://{workspace_root}/{path}": (
                raw_edits if isinstance(raw_edits, list) else []
            ),
        },
    }
    result = await apply_workspace_edit(edit, ctx, workspace_root=workspace_root)
    changed_paths = [
        str(path) for path in result.get("changed_paths", []) if isinstance(path, str)
    ]
    return {
        "success": bool(result.get("success")),
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "from_mounted_workspace_callback": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "changed_paths": changed_paths,
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.apply",
            "method": "textDocument/formatting",
            "path": path,
            "edits": raw_edits if isinstance(raw_edits, list) else [],
            "edit_count": len(raw_edits) if isinstance(raw_edits, list) else 0,
            "apply": result,
        },
    }


@register_plugin_op(
    "generic",
    "lsp_bridge_execute_command",
    intent=Intent.WRITE_ALLOWED,
    auto_workspace_overlay=False,
)
async def lsp_bridge_execute_command(
    args: dict[str, Any],
    ctx: Any,
) -> dict[str, Any]:
    from plugins.catalog.lsp.runtime.apply import apply_workspace_edit

    command = str(args.get("command") or "")
    commands = ["generic.applyWorkspaceEdit"]
    raw_arguments = args.get("arguments")
    first_argument = (
        raw_arguments[0]
        if isinstance(raw_arguments, list)
        and raw_arguments
        and isinstance(raw_arguments[0], dict)
        else {}
    )
    raw_edit = first_argument.get("edit") if isinstance(first_argument, dict) else None
    if command not in commands:
        return {
            "success": False,
            "from_lsp_importlib_bridge": True,
            "from_ppc_service_bridge": True,
            "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
            "protocol": "lsp-python-importlib",
            "method": "workspace/executeCommand",
            "command": command,
            "commands": commands,
            "supported": False,
            "unsupported": True,
            "error": "unsupported command",
        }
    result = await apply_workspace_edit(
        raw_edit if isinstance(raw_edit, dict) else {},
        ctx,
        workspace_root=str(ctx.overlay.workspace_root),
    )
    changed_paths = [
        str(path) for path in result.get("changed_paths", []) if isinstance(path, str)
    ]
    return {
        "success": bool(result.get("success")),
        "from_lsp_importlib_bridge": True,
        "from_ppc_service_bridge": True,
        "from_mounted_workspace_callback": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "changed_paths": changed_paths,
        "lsp": {
            "protocol": "lsp-python-importlib",
            "server": "plugins.catalog.lsp.runtime.apply",
            "method": "workspace/executeCommand",
            "command": command,
            "commands": commands,
            "supported": True,
            "unsupported": False,
            "arguments": raw_arguments if isinstance(raw_arguments, list) else [],
            "apply": result,
            "result": {
                "applied": bool(result.get("success")),
                "changed_paths": changed_paths,
            },
        },
    }


@register_plugin_op("generic", "runtime_bridge_delay_ping", intent=Intent.READ_ONLY)
async def runtime_bridge_delay_ping(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    delay_s = float(args.get("delay_s") or 0)
    started_at = time.monotonic()
    if delay_s > 0:
        await asyncio.sleep(delay_s)
    finished_at = time.monotonic()
    return {
        "success": True,
        "from_runtime_bridge": True,
        "from_ppc_service_bridge": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "manifest_key": ctx.overlay.active_manifest_key(),
        "echo": args.get("message"),
        "delay_s": delay_s,
        "service_started_at_s": started_at,
        "service_finished_at_s": finished_at,
    }


@register_plugin_op(
    "generic",
    "runtime_bridge_apply",
    intent=Intent.WRITE_ALLOWED,
    auto_workspace_overlay=False,
)
async def runtime_bridge_apply(args: dict[str, Any], ctx: Any) -> dict[str, Any]:
    rel_path = str(args.get("path") or "")
    if not rel_path:
        return {"success": False, "error": "missing path"}
    workspace_root = str(ctx.overlay.workspace_root)
    target = Path(workspace_root) / rel_path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(str(args.get("content") or ""), encoding="utf-8")
    callback = await ctx.overlay.publish_mounted_workspace_changes(
        [rel_path],
        workspace_root=workspace_root,
    )
    return {
        "success": bool(callback.get("success")),
        "from_runtime_bridge": True,
        "from_ppc_service_bridge": True,
        "from_mounted_workspace_callback": True,
        "workspace_mounted": os.environ.get("EOS_PLUGIN_WORKSPACE_MOUNTED") == "1",
        "workspace_root": workspace_root,
        "manifest_key": ctx.overlay.active_manifest_key(),
        "changed_paths": [rel_path],
        "callback": callback,
    }
'''

PYRIGHT_SETUP_SOURCE = r'''#!/usr/bin/env bash
set -eu

NODE_HOME="${EOS_NODE_HOME:-/tmp/eos-node22}"
NODE_VERSION="${EOS_NODE_VERSION:-22.13.1}"
PYRIGHT_VERSION="${EOS_PYRIGHT_VERSION:-1.1.409}"
MARKER="/eos/plugin/.rust_pyright_installed"
LOCK_DIR="/eos/plugin/.rust_pyright_setup.lock"
LOCK_OWNER="$LOCK_DIR/owner"
LOCK_STALE_AFTER_S="${EOS_PYRIGHT_SETUP_LOCK_STALE_AFTER_S:-900}"

export PATH="$NODE_HOME/bin:$PATH"

if [ -f "$MARKER" ] && command -v pyright-langserver >/dev/null 2>&1; then
    exit 0
fi

now_s() {
    date +%s
}

cleanup_stale_lock() {
    if [ ! -d "$LOCK_DIR" ]; then
        return 0
    fi
    owner_pid=""
    owner_started_s="0"
    if [ -f "$LOCK_OWNER" ]; then
        read -r owner_pid owner_started_s < "$LOCK_OWNER" || true
    fi
    if [ -n "$owner_pid" ] && kill -0 "$owner_pid" 2>/dev/null; then
        age_s=$(( $(now_s) - ${owner_started_s:-0} ))
        if [ "$age_s" -le "$LOCK_STALE_AFTER_S" ]; then
            return 0
        fi
    fi
    rm -rf "$LOCK_DIR"
}

while ! mkdir "$LOCK_DIR" 2>/dev/null; do
    if [ -f "$MARKER" ] && command -v pyright-langserver >/dev/null 2>&1; then
        exit 0
    fi
    cleanup_stale_lock
    sleep 0.2
done
printf '%s %s\n' "$$" "$(now_s)" > "$LOCK_OWNER"

cleanup_lock() {
    owner_pid=""
    if [ -f "$LOCK_OWNER" ]; then
        read -r owner_pid _ < "$LOCK_OWNER" || true
    fi
    if [ "$owner_pid" = "$$" ]; then
        rm -rf "$LOCK_DIR"
    fi
}
trap cleanup_lock EXIT INT TERM

if [ -f "$MARKER" ] && command -v pyright-langserver >/dev/null 2>&1; then
    exit 0
fi

download_node() {
    arch="$(uname -m)"
    case "$arch" in
        x86_64) node_arch=x64 ;;
        aarch64|arm64) node_arch=arm64 ;;
        *) echo "unsupported arch: $arch" >&2; return 2 ;;
    esac

    archive="node-v${NODE_VERSION}-linux-${node_arch}.tar.xz"
    urls="${EOS_NODE_DOWNLOAD_URLS:-https://registry.npmmirror.com/-/binary/node/v${NODE_VERSION}/${archive} https://nodejs.org/dist/v${NODE_VERSION}/${archive}}"
    mkdir -p "$NODE_HOME"
    cd "$NODE_HOME"
    for url in $urls; do
        rm -f node.tar.xz
        if curl -fL --retry 2 --connect-timeout 10 --max-time 240 "$url" -o node.tar.xz; then
            break
        fi
        echo "node download failed from $url" >&2
    done
    if [ ! -s node.tar.xz ]; then
        echo "failed to download Node ${NODE_VERSION}" >&2
        return 35
    fi
    tar --no-same-owner --no-same-permissions -xJf node.tar.xz --strip-components=1
}

if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
    download_node
fi

export PATH="$NODE_HOME/bin:$PATH"
cd "$NODE_HOME"
npm config set prefix "$NODE_HOME"
if ! command -v pyright-langserver >/dev/null 2>&1; then
    npm install -g --omit=optional "pyright@${PYRIGHT_VERSION}" || \
        npm --registry=https://registry.npmmirror.com install -g --omit=optional "pyright@${PYRIGHT_VERSION}"
fi

node -v
npm -v
pyright --version
command -v pyright-langserver >/dev/null

mkdir -p "$(dirname "$MARKER")"
: > "$MARKER"
'''

ONESHOT_SOURCE = r'''
from __future__ import annotations

import json
import os
from pathlib import Path


def main() -> int:
    request_path = Path(os.environ["EOS_PLUGIN_REQUEST_PATH"])
    result_path = Path(os.environ["EOS_PLUGIN_RESULT_PATH"])
    request = json.loads(request_path.read_text(encoding="utf-8"))
    args = request.get("args") or {}
    path = str(args.get("path") or "")
    content = str(args.get("content") or "")
    if not path:
        result_path.write_text(
            json.dumps({"success": False, "error": "missing path"}, sort_keys=True),
            encoding="utf-8",
        )
        return 2

    target = Path(os.environ["EOS_PLUGIN_WORKSPACE_ROOT"]) / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content, encoding="utf-8")
    result_path.write_text(
        json.dumps(
            {
                "success": True,
                "worker": "oneshot_overlay",
                "path": path,
                "op": os.environ.get("EOS_PLUGIN_PUBLIC_OP"),
                "manifest_version": request.get("manifest_version"),
            },
            sort_keys=True,
        ),
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
'''


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    report = asyncio.run(run(args))
    out = Path(args.report)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, sort_keys=True))
    if args.markdown_report:
        md_out = Path(args.markdown_report)
        md_out.parent.mkdir(parents=True, exist_ok=True)
        md_out.write_text(markdown_report(report))
    print(
        f"wrote {out} "
        f"(gate_pass={report['gate_pass']} run_id={report['run_id']} "
        f"container={report['sandbox_id']})"
    )
    return 0 if report["gate_pass"] else 1


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docker-image", default=DEFAULT_DOCKER_IMAGE)
    parser.add_argument("--container-id", default=None)
    parser.add_argument(
        "--artifact",
        type=Path,
        default=ROOT / "sandbox" / "dist" / "eosd-linux-amd64",
        help="Locally packaged amd64 eosd binary.",
    )
    parser.add_argument(
        "--report",
        default=str(ROOT / "bench" / "rust-daemon-plugin-generic.json"),
    )
    parser.add_argument("--markdown-report", default=None)
    parser.add_argument("--keep-container", action="store_true")
    parser.add_argument("--name-prefix", default="eos-rust-daemon-plugin")
    return parser.parse_args(argv)


async def run(args: argparse.Namespace) -> dict[str, Any]:
    if not args.artifact.exists():
        raise SystemExit(f"missing eosd artifact: {args.artifact}")
    bench = await DockerBench.create(
        image=args.docker_image,
        container_id=args.container_id,
        name_prefix=args.name_prefix,
    )
    try:
        report: dict[str, Any] = {
            "mode": "docker-existing-container-rust-daemon-generic-plugin",
            "run_id": os.environ.get("EOS_TIER_RUN_ID") or f"local-{uuid.uuid4().hex[:12]}",
            "sandbox_id": bench.sandbox_id,
            "created_container": bench.created,
            "host": {"platform": platform.platform(), "python": sys.version.split()[0]},
            "environment": await collect_environment(bench),
            "config": {
                "layer_stack_root": LAYER_STACK_ROOT,
                "workspace_root": WORKSPACE_ROOT,
                "harness_script": HARNESS_SCRIPT,
                "oneshot_script": ONESHOT_SCRIPT,
                "vanilla_package_script": VANILLA_PACKAGE_SCRIPT,
                "pyright_setup_script": PYRIGHT_SETUP_SCRIPT,
                "runtime_bridge_server": RUNTIME_BRIDGE_SERVER,
                "recover_marker": RECOVER_MARKER,
                "target": TARGET_REL,
                "runtime_bridge_target": RUNTIME_BRIDGE_TARGET_REL,
                "multi_targets": [MULTI_TARGET_A_REL, MULTI_TARGET_B_REL],
                "oneshot_target": ONESHOT_TARGET_REL,
                "pyright_target": PYRIGHT_TARGET_REL,
                "pyright_diagnostics_target": PYRIGHT_DIAGNOSTICS_TARGET_REL,
            },
        }
        await cleanup_processes(bench)
        await cleanup_experiment_files(bench)
        await reset_runtime(bench)
        report["artifact"] = await upload_artifact(bench, args.artifact)
        report["harness"] = await install_harness(bench)
        report["experiment_dirs"] = await prepare_experiment_dirs(bench)
        report["isolated_gate_environment"] = await configure_isolated_gate_environment(
            bench,
        )
        pyright_setup = await bench.exec(PYRIGHT_SETUP_SCRIPT, timeout=600)
        report["pyright_setup"] = result_block(pyright_setup)
        if (
            not report["harness"]["gate_pass"]
            or not report["experiment_dirs"]["gate_pass"]
            or not report["isolated_gate_environment"]["gate_pass"]
            or report["pyright_setup"]["exit_code"] != 0
        ):
            report["gate_pass"] = False
            return report

        with temporary_env("EOS_SANDBOX_RUNTIME", "rust"):
            from sandbox.host import daemon_client

            daemon_client.invalidate_daemon_tcp_endpoint(bench.sandbox_id)
            started = time.perf_counter()
            await daemon_client.ensure_daemon_current(bench.sandbox_id)
            report["daemon_spawn_ms"] = elapsed_ms(started)
            endpoint = await daemon_client._resolve_daemon_tcp_endpoint(  # noqa: SLF001
                bench.adapter,
                bench.sandbox_id,
            )
            if endpoint is None:
                raise RuntimeError("Docker sandbox did not expose a daemon TCP endpoint")
            report["endpoint"] = {
                "host": endpoint.host,
                "port": endpoint.port,
                "internal_port": endpoint.internal_port,
                "auth_token_present": bool(endpoint.auth_token),
            }
            report["workspace_base"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.build_workspace_base",
                {"workspace_root": WORKSPACE_ROOT, "reset": True},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=180,
            )
            report["ready"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.runtime.ready",
                {},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["ensure"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.ensure",
                {
                    "agent_id": AGENT_ID,
                    "workspace_root": WORKSPACE_ROOT,
                    "start_services": True,
                    "manifest": plugin_manifest(),
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["status_after_ensure"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["status_after_health_probe"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {
                    "agent_id": AGENT_ID,
                    "probe_services": True,
                    "probe_timeout_ms": 5000,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                report["health_fail_recover_ping"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.health_fail_recover_ping",
                    {"agent_id": AGENT_ID, "message": "after-health-fail-recover"},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            except Exception as exc:
                report["health_fail_recover_ping"] = {
                    "success": False,
                    "from_health_recovered_service": False,
                    "error": str(exc),
                }
            report["status_after_health_fail_recover"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["ping"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.ping",
                {"agent_id": AGENT_ID, "message": "hello"},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["concurrent_ping"] = list(
                await asyncio.gather(
                    daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        "plugin.generic.ping",
                        {"agent_id": AGENT_ID, "message": "concurrent-a"},
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=30,
                    ),
                    daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        "plugin.generic.ping",
                        {"agent_id": AGENT_ID, "message": "concurrent-b"},
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=30,
                    ),
                )
            )
            report["apply"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.apply",
                {
                    "agent_id": AGENT_ID,
                    "path": TARGET_REL,
                    "content": TARGET_CONTENT,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["runtime_bridge_ping"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.runtime_bridge_ping",
                {
                    "agent_id": AGENT_ID,
                    "read_path": TARGET_REL,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["status_after_runtime_bridge_ping"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )

            async def runtime_bridge_delay_call(
                message: str,
                delay_s: float,
            ) -> dict[str, Any]:
                started = time.perf_counter()
                response = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.runtime_bridge_delay_ping",
                    {
                        "agent_id": AGENT_ID,
                        "message": message,
                        "delay_s": delay_s,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
                finished = time.perf_counter()
                if isinstance(response, dict):
                    response["client_elapsed_s"] = finished - started
                    response["client_started_s"] = started
                    response["client_finished_s"] = finished
                return response

            report["runtime_bridge_concurrent"] = list(
                await asyncio.gather(
                    runtime_bridge_delay_call("slow-first", 0.35),
                    runtime_bridge_delay_call("fast-second", 0.0),
                )
            )
            report["runtime_bridge_apply"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.runtime_bridge_apply",
                {
                    "agent_id": AGENT_ID,
                    "path": RUNTIME_BRIDGE_TARGET_REL,
                    "content": RUNTIME_BRIDGE_CONTENT,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["runtime_bridge_readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": RUNTIME_BRIDGE_TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["runtime_bridge_concurrent_apply"] = list(
                await asyncio.gather(
                    daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        "plugin.generic.runtime_bridge_apply",
                        {
                            "agent_id": AGENT_ID,
                            "path": RUNTIME_BRIDGE_CONCURRENT_A_REL,
                            "content": RUNTIME_BRIDGE_CONCURRENT_A_CONTENT,
                        },
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=30,
                    ),
                    daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        "plugin.generic.runtime_bridge_apply",
                        {
                            "agent_id": AGENT_ID,
                            "path": RUNTIME_BRIDGE_CONCURRENT_B_REL,
                            "content": RUNTIME_BRIDGE_CONCURRENT_B_CONTENT,
                        },
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=30,
                    ),
                )
            )
            report["runtime_bridge_concurrent_readback_a"] = (
                await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "api.v1.read_file",
                    {"agent_id": AGENT_ID, "path": RUNTIME_BRIDGE_CONCURRENT_A_REL},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            )
            report["runtime_bridge_concurrent_readback_b"] = (
                await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "api.v1.read_file",
                    {"agent_id": AGENT_ID, "path": RUNTIME_BRIDGE_CONCURRENT_B_REL},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            )
            report["apply_multi"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.apply_multi",
                {
                    "agent_id": AGENT_ID,
                    "writes": [
                        {
                            "path": MULTI_TARGET_A_REL,
                            "content": MULTI_TARGET_A_CONTENT,
                        },
                        {
                            "path": MULTI_TARGET_B_REL,
                            "content": MULTI_TARGET_B_CONTENT,
                        },
                    ],
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["multi_readback_a"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": MULTI_TARGET_A_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["multi_readback_b"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": MULTI_TARGET_B_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["shell_publish"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.exec_command",
                {
                    "agent_id": AGENT_ID,
                    "cmd": (
                        f"printf %s {shlex.quote(SHELL_CONTENT)} > "
                        f"{shlex.quote(f'{WORKSPACE_ROOT}/{SHELL_TARGET_REL}')}"
                    ),
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["shell_readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": SHELL_TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["shell_refresh_ping"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.ping",
                {
                    "agent_id": AGENT_ID,
                    "message": "after-shell-publish",
                    "read_path": SHELL_TARGET_REL,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["status_after_shell_refresh"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["refresh_ping"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.ping",
                {
                    "agent_id": AGENT_ID,
                    "message": "after-write",
                    "read_path": TARGET_REL,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["status_after_refresh"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["adapter_query"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.adapter_query",
                {
                    "agent_id": AGENT_ID,
                    "read_path": TARGET_REL,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["status_after_adapter"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["co_shared_refresh"] = co_shared_refresh_summary(
                report["status_after_adapter"],
                "harness",
                "adapter_harness",
            )
            report["lsp_bridge_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{LSP_BRIDGE_TARGET_REL}",
                    "content": LSP_BRIDGE_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["lsp_bridge_apply_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{LSP_BRIDGE_APPLY_TARGET_REL}",
                    "content": LSP_BRIDGE_APPLY_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["lsp_bridge_code_action_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{LSP_BRIDGE_CODE_ACTION_TARGET_REL}",
                    "content": LSP_BRIDGE_CODE_ACTION_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["lsp_bridge_format_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{LSP_BRIDGE_FORMAT_TARGET_REL}",
                    "content": LSP_BRIDGE_FORMAT_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["lsp_bridge_execute_command_seed"] = (
                await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "api.v1.write_file",
                    {
                        "agent_id": AGENT_ID,
                        "path": (
                            f"{WORKSPACE_ROOT}/"
                            f"{LSP_BRIDGE_EXECUTE_COMMAND_TARGET_REL}"
                        ),
                        "content": LSP_BRIDGE_EXECUTE_COMMAND_CONTENT,
                        "overwrite": True,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            )
            report["lsp_bridge_diagnostics_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{LSP_BRIDGE_DIAGNOSTICS_TARGET_REL}",
                    "content": LSP_BRIDGE_DIAGNOSTICS_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["pyright_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{PYRIGHT_TARGET_REL}",
                    "content": PYRIGHT_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["pyright_completion_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{PYRIGHT_COMPLETION_TARGET_REL}",
                    "content": PYRIGHT_COMPLETION_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["pyright_diagnostics_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{PYRIGHT_DIAGNOSTICS_TARGET_REL}",
                    "content": PYRIGHT_DIAGNOSTICS_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["pyright_code_action_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{PYRIGHT_CODE_ACTION_TARGET_REL}",
                    "content": PYRIGHT_CODE_ACTION_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["lsp_apply_workspace_edit_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{LSP_APPLY_EDIT_TARGET_REL}",
                    "content": LSP_APPLY_EDIT_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["lsp_apply_code_action_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{LSP_APPLY_CODE_ACTION_TARGET_REL}",
                    "content": LSP_APPLY_CODE_ACTION_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["lsp_format_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{LSP_FORMAT_TARGET_REL}",
                    "content": LSP_FORMAT_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["lsp_execute_command_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{LSP_EXECUTE_COMMAND_TARGET_REL}",
                    "content": LSP_EXECUTE_COMMAND_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["pyright_signature_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{PYRIGHT_SIGNATURE_TARGET_REL}",
                    "content": PYRIGHT_SIGNATURE_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["pyright_type_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{PYRIGHT_TYPE_TARGET_REL}",
                    "content": PYRIGHT_TYPE_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["pyright_call_hierarchy_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{PYRIGHT_CALL_HIERARCHY_TARGET_REL}",
                    "content": PYRIGHT_CALL_HIERARCHY_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                report["pyright_symbols"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_symbols",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                        "query": PYRIGHT_SYMBOL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_symbols"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_workspace_symbols"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_workspace_symbols",
                    {
                        "agent_id": AGENT_ID,
                        "query": PYRIGHT_SYMBOL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_workspace_symbols"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_capabilities"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_capabilities",
                    {
                        "agent_id": AGENT_ID,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_capabilities"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_document_formatting"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_document_formatting",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_document_formatting"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_execute_command"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_execute_command",
                    {
                        "agent_id": AGENT_ID,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_execute_command"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_completion"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_completion",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_COMPLETION_TARGET_REL,
                        "line": PYRIGHT_COMPLETION_LINE,
                        "character": PYRIGHT_COMPLETION_CHARACTER,
                        "query": PYRIGHT_SYMBOL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_completion"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_completion_resolve"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_completion_resolve",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_COMPLETION_TARGET_REL,
                        "line": PYRIGHT_COMPLETION_LINE,
                        "character": PYRIGHT_COMPLETION_CHARACTER,
                        "query": PYRIGHT_SYMBOL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_completion_resolve"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_diagnostics"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_diagnostics",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_DIAGNOSTICS_TARGET_REL,
                        "line": PYRIGHT_DIAGNOSTICS_LINE,
                        "character": PYRIGHT_DIAGNOSTICS_CHARACTER,
                        "query": PYRIGHT_DIAGNOSTICS_SYMBOL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_diagnostics"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_code_actions"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_code_actions",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_CODE_ACTION_TARGET_REL,
                        "line": PYRIGHT_CODE_ACTION_LINE,
                        "character": PYRIGHT_CODE_ACTION_CHARACTER,
                        "only": [PYRIGHT_CODE_ACTION_KIND],
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_code_actions"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_signature_help"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_signature_help",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_SIGNATURE_TARGET_REL,
                        "line": PYRIGHT_SIGNATURE_LINE,
                        "character": PYRIGHT_SIGNATURE_CHARACTER,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_signature_help"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_hover"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_hover",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                        "line": 3,
                        "character": 12,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_hover"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_type_definition"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_type_definition",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TYPE_TARGET_REL,
                        "line": PYRIGHT_TYPE_LINE,
                        "character": PYRIGHT_TYPE_CHARACTER,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_type_definition"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_declaration"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_declaration",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                        "line": 3,
                        "character": 12,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_declaration"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_call_hierarchy"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_call_hierarchy",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_CALL_HIERARCHY_TARGET_REL,
                        "line": PYRIGHT_CALL_HIERARCHY_LINE,
                        "character": PYRIGHT_CALL_HIERARCHY_CHARACTER,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_call_hierarchy"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_call_hierarchy_outgoing"] = (
                    await daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        "plugin.generic.pyright_call_hierarchy",
                        {
                            "agent_id": AGENT_ID,
                            "read_path": PYRIGHT_CALL_HIERARCHY_TARGET_REL,
                            "line": PYRIGHT_CALL_HIERARCHY_OUTGOING_LINE,
                            "character": PYRIGHT_CALL_HIERARCHY_OUTGOING_CHARACTER,
                        },
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=150,
                    )
                )
            except Exception as exc:
                report["pyright_call_hierarchy_outgoing"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_document_highlight"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_document_highlight",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                        "line": 3,
                        "character": 12,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_document_highlight"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_prepare_rename"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_prepare_rename",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                        "line": 3,
                        "character": 12,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_prepare_rename"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_definition"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_definition",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                        "line": 3,
                        "character": 12,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_definition"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            try:
                report["pyright_references"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_references",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                        "line": 3,
                        "character": 12,
                        "include_declaration": True,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_references"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "error": str(exc),
                }
            lsp_apply_edit_uri = f"file://{WORKSPACE_ROOT}/{LSP_APPLY_EDIT_TARGET_REL}"
            try:
                report["lsp_apply_workspace_edit"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.lsp_apply_workspace_edit",
                    {
                        "agent_id": AGENT_ID,
                        "edit": {
                            "changes": {
                                lsp_apply_edit_uri: [
                                    {
                                        "range": {
                                            "start": {"line": 1, "character": 0},
                                            "end": {"line": 1, "character": 4},
                                        },
                                        "newText": LSP_APPLY_EDIT_REPLACEMENT,
                                    }
                                ]
                            }
                        },
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["lsp_apply_workspace_edit"] = {
                    "success": False,
                    "from_lsp_workspace_edit": False,
                    "from_self_managed": False,
                    "error": str(exc),
                }
            report["lsp_apply_workspace_edit_readback"] = (
                await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "api.v1.read_file",
                    {"agent_id": AGENT_ID, "path": LSP_APPLY_EDIT_TARGET_REL},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            )
            lsp_apply_code_action_uri = (
                f"file://{WORKSPACE_ROOT}/{LSP_APPLY_CODE_ACTION_TARGET_REL}"
            )
            try:
                report["lsp_apply_code_action"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.lsp_apply_code_action",
                    {
                        "agent_id": AGENT_ID,
                        "action": {
                            "title": LSP_APPLY_CODE_ACTION_TITLE,
                            "kind": LSP_APPLY_CODE_ACTION_KIND,
                            "edit": {
                                "changes": {
                                    lsp_apply_code_action_uri: [
                                        {
                                            "range": {
                                                "start": {
                                                    "line": 0,
                                                    "character": 0,
                                                },
                                                "end": {
                                                    "line": 0,
                                                    "character": 6,
                                                },
                                            },
                                            "newText": LSP_APPLY_CODE_ACTION_REPLACEMENT,
                                        }
                                    ]
                                }
                            },
                        },
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["lsp_apply_code_action"] = {
                    "success": False,
                    "from_lsp_code_action": False,
                    "from_self_managed": False,
                    "error": str(exc),
                }
            report["lsp_apply_code_action_readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": LSP_APPLY_CODE_ACTION_TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                report["lsp_format_document"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.lsp_format_document",
                    {
                        "agent_id": AGENT_ID,
                        "path": LSP_FORMAT_TARGET_REL,
                        "edits": [
                            {
                                "range": {
                                    "start": {"line": 0, "character": 0},
                                    "end": {"line": 2, "character": 0},
                                },
                                "newText": LSP_FORMAT_CONTENT_AFTER,
                            }
                        ],
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["lsp_format_document"] = {
                    "success": False,
                    "from_lsp_formatting": False,
                    "from_self_managed": False,
                    "error": str(exc),
                }
            report["lsp_format_readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": LSP_FORMAT_TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            lsp_execute_command_uri = (
                f"file://{WORKSPACE_ROOT}/{LSP_EXECUTE_COMMAND_TARGET_REL}"
            )
            try:
                report["lsp_execute_command"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.lsp_execute_command",
                    {
                        "agent_id": AGENT_ID,
                        "command": LSP_EXECUTE_COMMAND_NAME,
                        "arguments": [
                            {
                                "edit": {
                                    "changes": {
                                        lsp_execute_command_uri: [
                                            {
                                                "range": {
                                                    "start": {
                                                        "line": 0,
                                                        "character": 0,
                                                    },
                                                    "end": {
                                                        "line": 0,
                                                        "character": len(
                                                            LSP_EXECUTE_COMMAND_CONTENT.strip()
                                                        ),
                                                    },
                                                },
                                                "newText": (
                                                    LSP_EXECUTE_COMMAND_CONTENT_AFTER.strip()
                                                ),
                                            }
                                        ]
                                    }
                                }
                            }
                        ],
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["lsp_execute_command"] = {
                    "success": False,
                    "from_lsp_execute_command": False,
                    "from_self_managed": False,
                    "error": str(exc),
                }
            report["lsp_execute_command_readback"] = (
                await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "api.v1.read_file",
                    {"agent_id": AGENT_ID, "path": LSP_EXECUTE_COMMAND_TARGET_REL},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            )
            report["status_after_pyright"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                report["lsp_bridge_rename"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.lsp_bridge_rename",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": LSP_BRIDGE_TARGET_REL,
                        "line": 3,
                        "character": len("RESULT = bri"),
                        "new_name": LSP_BRIDGE_RENAMED_SYMBOL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["lsp_bridge_rename"] = {
                    "success": False,
                    "from_lsp_importlib_bridge": False,
                    "error": str(exc),
                }
            report["lsp_bridge_rename_readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": LSP_BRIDGE_TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                report["lsp_bridge_query_symbols"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.lsp_bridge_query_symbols",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": LSP_BRIDGE_TARGET_REL,
                        "query": LSP_BRIDGE_RENAMED_SYMBOL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["lsp_bridge_query_symbols"] = {
                    "success": False,
                    "from_lsp_importlib_bridge": False,
                    "error": str(exc),
                }
            lsp_bridge_position = {
                "line": 3,
                "character": len("RESULT = bri"),
            }
            lsp_bridge_diagnostics_position = {
                "line": LSP_BRIDGE_DIAGNOSTICS_LINE,
                "character": LSP_BRIDGE_DIAGNOSTICS_CHARACTER,
            }
            lsp_bridge_signature_position = {
                "line": PYRIGHT_SIGNATURE_LINE,
                "character": PYRIGHT_SIGNATURE_CHARACTER,
            }
            lsp_bridge_highlight_position = {
                "line": 3,
                "character": len("RESULT = liv"),
            }

            async def call_lsp_bridge_read_only(
                public_op: str,
                args: dict[str, Any],
            ) -> dict[str, Any]:
                try:
                    return await daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        public_op,
                        args,
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=150,
                    )
                except Exception as exc:
                    return {
                        "success": False,
                        "from_lsp_importlib_bridge": False,
                        "from_ppc_service_bridge": False,
                        "error": str(exc),
                    }

            (
                report["lsp_bridge_find_definitions"],
                report["lsp_bridge_find_references"],
                report["lsp_bridge_signature_help"],
                report["lsp_bridge_document_highlight"],
                report["lsp_bridge_diagnostics"],
                report["lsp_bridge_code_actions"],
            ) = await asyncio.gather(
                call_lsp_bridge_read_only(
                    "plugin.generic.lsp_bridge_find_definitions",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": LSP_BRIDGE_TARGET_REL,
                        **lsp_bridge_position,
                    },
                ),
                call_lsp_bridge_read_only(
                    "plugin.generic.lsp_bridge_find_references",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": LSP_BRIDGE_TARGET_REL,
                        **lsp_bridge_position,
                        "include_declaration": True,
                    },
                ),
                call_lsp_bridge_read_only(
                    "plugin.generic.lsp_bridge_signature_help",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_SIGNATURE_TARGET_REL,
                        **lsp_bridge_signature_position,
                    },
                ),
                call_lsp_bridge_read_only(
                    "plugin.generic.lsp_bridge_document_highlight",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                        **lsp_bridge_highlight_position,
                    },
                ),
                call_lsp_bridge_read_only(
                    "plugin.generic.lsp_bridge_diagnostics",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": LSP_BRIDGE_DIAGNOSTICS_TARGET_REL,
                        **lsp_bridge_diagnostics_position,
                        "wait_for_diagnostics": True,
                    },
                ),
                call_lsp_bridge_read_only(
                    "plugin.generic.lsp_bridge_code_actions",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": LSP_BRIDGE_DIAGNOSTICS_TARGET_REL,
                        **lsp_bridge_diagnostics_position,
                        "only": [LSP_BRIDGE_CODE_ACTION_KIND],
                    },
                ),
            )
            try:
                report["lsp_bridge_hover"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.lsp_bridge_hover",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": LSP_BRIDGE_TARGET_REL,
                        **lsp_bridge_position,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["lsp_bridge_hover"] = {
                    "success": False,
                    "from_lsp_importlib_bridge": False,
                    "error": str(exc),
                }
            lsp_bridge_apply_uri = (
                f"file://{WORKSPACE_ROOT}/{LSP_BRIDGE_APPLY_TARGET_REL}"
            )
            try:
                report["lsp_bridge_apply_workspace_edit"] = (
                    await daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        "plugin.generic.lsp_bridge_apply_workspace_edit",
                        {
                            "agent_id": AGENT_ID,
                            "edit": {
                                "changes": {
                                    lsp_bridge_apply_uri: [
                                        {
                                            "range": {
                                                "start": {
                                                    "line": 0,
                                                    "character": 0,
                                                },
                                                "end": {
                                                    "line": 0,
                                                    "character": len(
                                                        LSP_BRIDGE_APPLY_CONTENT.rstrip(
                                                            "\n"
                                                        )
                                                    ),
                                                },
                                            },
                                            "newText": LSP_BRIDGE_APPLY_CONTENT_AFTER.rstrip(
                                                "\n"
                                            ),
                                        }
                                    ]
                                }
                            },
                        },
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=150,
                    )
                )
            except Exception as exc:
                report["lsp_bridge_apply_workspace_edit"] = {
                    "success": False,
                    "from_lsp_importlib_bridge": False,
                    "error": str(exc),
                }
            report["lsp_bridge_apply_readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": LSP_BRIDGE_APPLY_TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            lsp_bridge_code_action_uri = (
                f"file://{WORKSPACE_ROOT}/{LSP_BRIDGE_CODE_ACTION_TARGET_REL}"
            )
            try:
                report["lsp_bridge_apply_code_action"] = (
                    await daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        "plugin.generic.lsp_bridge_apply_code_action",
                        {
                            "agent_id": AGENT_ID,
                            "action": {
                                "title": LSP_BRIDGE_CODE_ACTION_TITLE,
                                "kind": LSP_BRIDGE_CODE_ACTION_KIND,
                                "edit": {
                                    "changes": {
                                        lsp_bridge_code_action_uri: [
                                            {
                                                "range": {
                                                    "start": {
                                                        "line": 0,
                                                        "character": 0,
                                                    },
                                                    "end": {
                                                        "line": 0,
                                                        "character": len(
                                                            LSP_BRIDGE_CODE_ACTION_CONTENT.rstrip(
                                                                "\n"
                                                            )
                                                        ),
                                                    },
                                                },
                                                "newText": LSP_BRIDGE_CODE_ACTION_CONTENT_AFTER.rstrip(
                                                    "\n"
                                                ),
                                            }
                                        ]
                                    }
                                },
                            },
                        },
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=150,
                    )
                )
            except Exception as exc:
                report["lsp_bridge_apply_code_action"] = {
                    "success": False,
                    "from_lsp_importlib_bridge": False,
                    "error": str(exc),
                }
            report["lsp_bridge_code_action_readback"] = (
                await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "api.v1.read_file",
                    {"agent_id": AGENT_ID, "path": LSP_BRIDGE_CODE_ACTION_TARGET_REL},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            )
            try:
                report["lsp_bridge_format_document"] = (
                    await daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        "plugin.generic.lsp_bridge_format_document",
                        {
                            "agent_id": AGENT_ID,
                            "path": LSP_BRIDGE_FORMAT_TARGET_REL,
                            "edits": [
                                {
                                    "range": {
                                        "start": {"line": 0, "character": 0},
                                        "end": {"line": 2, "character": 0},
                                    },
                                    "newText": LSP_BRIDGE_FORMAT_CONTENT_AFTER,
                                }
                            ],
                        },
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=150,
                    )
                )
            except Exception as exc:
                report["lsp_bridge_format_document"] = {
                    "success": False,
                    "from_lsp_importlib_bridge": False,
                    "error": str(exc),
                }
            report["lsp_bridge_format_readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": LSP_BRIDGE_FORMAT_TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            lsp_bridge_execute_command_uri = (
                f"file://{WORKSPACE_ROOT}/{LSP_BRIDGE_EXECUTE_COMMAND_TARGET_REL}"
            )
            try:
                report["lsp_bridge_execute_command"] = (
                    await daemon_client.call_daemon_api(
                        bench.sandbox_id,
                        "plugin.generic.lsp_bridge_execute_command",
                        {
                            "agent_id": AGENT_ID,
                            "command": LSP_BRIDGE_EXECUTE_COMMAND_NAME,
                            "arguments": [
                                {
                                    "edit": {
                                        "changes": {
                                            lsp_bridge_execute_command_uri: [
                                                {
                                                    "range": {
                                                        "start": {
                                                            "line": 0,
                                                            "character": 0,
                                                        },
                                                        "end": {
                                                            "line": 0,
                                                            "character": len(
                                                                LSP_BRIDGE_EXECUTE_COMMAND_CONTENT.strip()
                                                            ),
                                                        },
                                                    },
                                                    "newText": (
                                                        LSP_BRIDGE_EXECUTE_COMMAND_CONTENT_AFTER.strip()
                                                    ),
                                                }
                                            ]
                                        }
                                    }
                                }
                            ],
                        },
                        layer_stack_root=LAYER_STACK_ROOT,
                        timeout=150,
                    )
                )
            except Exception as exc:
                report["lsp_bridge_execute_command"] = {
                    "success": False,
                    "from_lsp_importlib_bridge": False,
                    "error": str(exc),
                }
            report["lsp_bridge_execute_command_readback"] = (
                await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "api.v1.read_file",
                    {
                        "agent_id": AGENT_ID,
                        "path": LSP_BRIDGE_EXECUTE_COMMAND_TARGET_REL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            )
            try:
                report["pyright_rename"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.pyright_rename",
                    {
                        "agent_id": AGENT_ID,
                        "read_path": PYRIGHT_TARGET_REL,
                        "line": 0,
                        "character": 4,
                        "new_name": PYRIGHT_RENAMED_SYMBOL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=150,
                )
            except Exception as exc:
                report["pyright_rename"] = {
                    "success": False,
                    "from_pyright_adapter": False,
                    "from_self_managed": False,
                    "error": str(exc),
                }
            report["pyright_rename_readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": PYRIGHT_TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["status_after_pyright_rename"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["restart_ping"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.restart_ping",
                {
                    "agent_id": AGENT_ID,
                    "message": "after-write-restart",
                    "read_path": TARGET_REL,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["status_after_restart"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["oneshot"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "plugin.generic.oneshot_write",
                {
                    "agent_id": AGENT_ID,
                    "path": ONESHOT_TARGET_REL,
                    "content": ONESHOT_CONTENT,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["oneshot_readback"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.read_file",
                {"agent_id": AGENT_ID, "path": ONESHOT_TARGET_REL},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                crash_response = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.crash_probe",
                    {"agent_id": AGENT_ID},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
                report["crash_probe"] = {
                    **crash_response,
                    "expected_failure": crash_response.get("success") is not True,
                }
            except Exception as exc:
                report["crash_probe"] = {
                    "success": False,
                    "expected_failure": True,
                    "error": str(exc),
                }
            report["status_after_crash"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["crash_recover_seed"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.v1.write_file",
                {
                    "agent_id": AGENT_ID,
                    "path": f"{WORKSPACE_ROOT}/{CRASH_RECOVERY_TARGET_REL}",
                    "content": CRASH_RECOVERY_CONTENT,
                    "overwrite": True,
                },
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                report["crash_recover_ping"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.crash_recover_ping",
                    {
                        "agent_id": AGENT_ID,
                        "message": "after-crash-recover",
                        "read_path": CRASH_RECOVERY_TARGET_REL,
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            except Exception as exc:
                report["crash_recover_ping"] = {
                    "success": False,
                    "from_crash_recovered_service": False,
                    "error": str(exc),
                }
            report["status_after_crash_recover"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                hang_response = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.hang_probe",
                    {"agent_id": AGENT_ID, "sleep_s": 10.0},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
                report["hang_probe"] = {
                    **hang_response,
                    "expected_failure": hang_response.get("success") is not True,
                }
            except Exception as exc:
                report["hang_probe"] = {
                    "success": False,
                    "expected_failure": True,
                    "error": str(exc),
                }
            report["status_after_hang"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                report["hang_recover_ping"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.hang_recover_ping",
                    {
                        "agent_id": AGENT_ID,
                        "message": "after-timeout-recover",
                    },
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            except Exception as exc:
                report["hang_recover_ping"] = {
                    "success": False,
                    "from_timeout_recovered_service": False,
                    "error": str(exc),
                }
            report["status_after_hang_recover"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                recover_first = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.recover_probe",
                    {"agent_id": AGENT_ID},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
                report["recover_probe_first"] = {
                    **recover_first,
                    "expected_failure": recover_first.get("success") is not True,
                }
            except Exception as exc:
                report["recover_probe_first"] = {
                    "success": False,
                    "expected_failure": True,
                    "error": str(exc),
                }
            report["status_after_recover_failure"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            try:
                report["recover_probe_second"] = await daemon_client.call_daemon_api(
                    bench.sandbox_id,
                    "plugin.generic.recover_probe",
                    {"agent_id": AGENT_ID},
                    layer_stack_root=LAYER_STACK_ROOT,
                    timeout=30,
                )
            except Exception as exc:
                report["recover_probe_second"] = {
                    "success": False,
                    "from_recovered_service": False,
                    "error": str(exc),
                }
            report["status_after_recover"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["isolated_plugin_gate"] = await probe_isolated_plugin_gate(
                daemon_client,
                bench.sandbox_id,
            )
            report["final_metrics"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.layer_metrics",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["processes_before_cleanup"] = await process_snapshot(bench)
            await cleanup_processes(bench)
            await asyncio.sleep(0.5)
            report["status_after_cleanup"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.plugin.status",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["post_cleanup_metrics"] = await daemon_client.call_daemon_api(
                bench.sandbox_id,
                "api.layer_metrics",
                {"agent_id": AGENT_ID},
                layer_stack_root=LAYER_STACK_ROOT,
                timeout=30,
            )
            report["processes_after_cleanup"] = await process_snapshot(bench)

        report["harness_log"] = await read_harness_log(bench)
        report["gate_pass"] = gate_pass(report)
        return report
    finally:
        try:
            await cleanup_processes(bench)
            await reset_runtime(bench)
        finally:
            await bench.close(keep=args.keep_container)


def ppc_service_command() -> list[str]:
    launcher = (
        "import sys; "
        f"sys.path.insert(0, {BUNDLE_REMOTE_DIR!r}); "
        "from sandbox.ephemeral_workspace.plugin.ppc_service import main; "
        "raise SystemExit(main())"
    )
    return ["python3", "-c", launcher]


def plugin_manifest() -> dict[str, Any]:
    return {
        "plugin_id": "generic",
        "plugin_version": "0.1.0",
        "plugin_digest": "generic-live-digest-v1",
        "services": [
            {
                "service_id": "harness",
                "service_profile_digest": "generic-harness-profile-v1",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace_and_notify",
                "command": ["python3", HARNESS_SCRIPT],
                "ppc_protocol_version": 1,
            },
            {
                "service_id": "restart_harness",
                "service_profile_digest": "generic-restart-profile-v1",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "restart_service",
                "command": ["python3", HARNESS_SCRIPT],
                "ppc_protocol_version": 1,
            },
            {
                "service_id": "adapter_harness",
                "service_profile_digest": "generic-adapter-profile-v1",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace",
                "command": ["python3", HARNESS_SCRIPT],
                "ppc_protocol_version": 1,
            },
            {
                "service_id": "runtime_bridge",
                "service_profile_digest": "generic-runtime-bridge-profile-v1",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace_and_notify",
                "command": ppc_service_command(),
                "ppc_protocol_version": 1,
            },
            {
                "service_id": "pyright_harness",
                "service_profile_digest": "generic-pyright-profile-v1",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace_and_notify",
                "command": ["python3", HARNESS_SCRIPT],
                "ppc_protocol_version": 1,
            },
            {
                "service_id": "crash_harness",
                "service_profile_digest": "generic-crash-profile-v1",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace",
                "command": ["python3", HARNESS_SCRIPT],
                "ppc_protocol_version": 1,
            },
            {
                "service_id": "hang_harness",
                "service_profile_digest": "generic-hang-profile-v1",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace",
                "command": ["python3", HARNESS_SCRIPT],
                "ppc_protocol_version": 1,
            },
            {
                "service_id": "recover_harness",
                "service_profile_digest": "generic-recover-profile-v1",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace",
                "command": ["python3", HARNESS_SCRIPT],
                "ppc_protocol_version": 1,
            },
            {
                "service_id": "health_fail_harness",
                "service_profile_digest": "generic-health-fail-profile-v1",
                "service_mode": "workspace_snapshot_refresh",
                "refresh_strategy": "remount_workspace",
                "command": ["python3", HARNESS_SCRIPT],
                "ppc_protocol_version": 1,
            },
            {
                "service_id": "oneshot",
                "service_profile_digest": "generic-oneshot-profile-v1",
                "service_mode": "oneshot_overlay",
                "refresh_strategy": "restart_service",
                "command": ["python3", ONESHOT_SCRIPT],
                "ppc_protocol_version": 1,
            }
        ],
        "operations": [
            {
                "op_name": "ping",
                "intent": "read_only",
                "service_id": "harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "restart_ping",
                "intent": "read_only",
                "service_id": "restart_harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "adapter_query",
                "intent": "read_only",
                "service_id": "adapter_harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "runtime_bridge_ping",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 10000,
            },
            {
                "op_name": "runtime_bridge_delay_ping",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 10000,
            },
            {
                "op_name": "runtime_bridge_apply",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "runtime_bridge",
                "timeout_ms": 10000,
            },
            {
                "op_name": "lsp_bridge_query_symbols",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_find_definitions",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_find_references",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_signature_help",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_document_highlight",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_diagnostics",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_code_actions",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_hover",
                "intent": "read_only",
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_rename",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_apply_workspace_edit",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_apply_code_action",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_format_document",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_bridge_execute_command",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "runtime_bridge",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_symbols",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_workspace_symbols",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_capabilities",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_document_formatting",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_execute_command",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_completion",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_completion_resolve",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_diagnostics",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_code_actions",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_signature_help",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_hover",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_type_definition",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_declaration",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_call_hierarchy",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_document_highlight",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_prepare_rename",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_definition",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_references",
                "intent": "read_only",
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "pyright_rename",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_apply_workspace_edit",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_apply_code_action",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_format_document",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "lsp_execute_command",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "pyright_harness",
                "timeout_ms": 150000,
            },
            {
                "op_name": "crash_probe",
                "intent": "read_only",
                "service_id": "crash_harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "crash_recover_ping",
                "intent": "read_only",
                "service_id": "crash_harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "hang_probe",
                "intent": "read_only",
                "service_id": "hang_harness",
                "timeout_ms": 1000,
            },
            {
                "op_name": "hang_recover_ping",
                "intent": "read_only",
                "service_id": "hang_harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "recover_probe",
                "intent": "read_only",
                "service_id": "recover_harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "health_fail_ping",
                "intent": "read_only",
                "service_id": "health_fail_harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "health_fail_recover_ping",
                "intent": "read_only",
                "service_id": "health_fail_harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "apply",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "apply_multi",
                "intent": "write_allowed",
                "auto_workspace_overlay": False,
                "service_id": "harness",
                "timeout_ms": 5000,
            },
            {
                "op_name": "oneshot_write",
                "intent": "write_allowed",
                "service_id": "oneshot",
                "timeout_ms": 10000,
            },
        ],
    }


async def install_harness(bench: DockerBench) -> dict[str, Any]:
    harness_payload = HARNESS_SOURCE.encode("utf-8")
    oneshot_payload = ONESHOT_SOURCE.encode("utf-8")
    vanilla_package_payload = VANILLA_PACKAGE_SOURCE.encode("utf-8")
    runtime_bridge_payload = RUNTIME_BRIDGE_SERVER_SOURCE.encode("utf-8")
    pyright_setup_payload = PYRIGHT_SETUP_SOURCE.encode("utf-8")
    ppc_service_bundle_bytes: dict[str, int] = {}
    staging_dir = f"/tmp/eos-plugin-harness-{uuid.uuid4().hex}"
    staging_file = f"{staging_dir}/rust_ppc_harness.py"
    staging_oneshot = f"{staging_dir}/rust_oneshot_worker.py"
    staging_vanilla_package = f"{staging_dir}/rust_vanilla_package.py"
    staging_runtime_bridge = f"{staging_dir}/runtime_bridge_server.py"
    staging_pyright_setup = f"{staging_dir}/rust_pyright_setup.sh"
    ppc_service_bundle_installs: list[str] = []
    mkdir = await bench.exec(
        f"mkdir -p {shlex.quote(staging_dir)} {shlex.quote(BUNDLE_REMOTE_DIR)}",
        timeout=30,
    )
    if getattr(mkdir, "exit_code", 1) != 0:
        return {
            "path": HARNESS_SCRIPT,
            "oneshot_path": ONESHOT_SCRIPT,
            "vanilla_package_path": VANILLA_PACKAGE_SCRIPT,
            "pyright_setup_path": PYRIGHT_SETUP_SCRIPT,
            "bytes": {
                "harness": len(harness_payload),
                "oneshot": len(oneshot_payload),
                "vanilla_package": len(vanilla_package_payload),
                "runtime_bridge": len(runtime_bridge_payload),
                "pyright_setup": len(pyright_setup_payload),
                "ppc_service_bundle": ppc_service_bundle_bytes,
            },
            "mkdir": result_block(mkdir),
            "gate_pass": False,
        }
    await bench.adapter.put_archive(
        bench.sandbox_id,
        tar_stream=tar_file_at_path("rust_ppc_harness.py", harness_payload, mode=0o755),
        dest_dir=staging_dir,
    )
    await bench.adapter.put_archive(
        bench.sandbox_id,
        tar_stream=tar_file_at_path("rust_oneshot_worker.py", oneshot_payload, mode=0o755),
        dest_dir=staging_dir,
    )
    await bench.adapter.put_archive(
        bench.sandbox_id,
        tar_stream=tar_file_at_path(
            "rust_vanilla_package.py",
            vanilla_package_payload,
            mode=0o755,
        ),
        dest_dir=staging_dir,
    )
    await bench.adapter.put_archive(
        bench.sandbox_id,
        tar_stream=tar_file_at_path(
            "runtime_bridge_server.py",
            runtime_bridge_payload,
            mode=0o644,
        ),
        dest_dir=staging_dir,
    )
    await bench.adapter.put_archive(
        bench.sandbox_id,
        tar_stream=tar_file_at_path(
            "rust_pyright_setup.sh",
            pyright_setup_payload,
            mode=0o755,
        ),
        dest_dir=staging_dir,
    )
    for relative_path in PPC_SERVICE_BUNDLE_FILES:
        payload = (BACKEND_SRC / relative_path).read_bytes()
        ppc_service_bundle_bytes[relative_path] = len(payload)
        staging_name = f"ppc_service_{relative_path.replace('/', '__')}"
        staging_path = f"{staging_dir}/{staging_name}"
        remote_path = f"{BUNDLE_REMOTE_DIR}/{relative_path}"
        await bench.adapter.put_archive(
            bench.sandbox_id,
            tar_stream=tar_file_at_path(staging_name, payload, mode=0o644),
            dest_dir=staging_dir,
        )
        ppc_service_bundle_installs.append(
            f"mkdir -p {shlex.quote(str(Path(remote_path).parent))} && "
            f"cat {shlex.quote(staging_path)} > {shlex.quote(remote_path)} && "
            f"chmod 644 {shlex.quote(remote_path)}"
        )
    bundle_install = " && ".join(ppc_service_bundle_installs)
    finalize = await bench.exec(
        f"mkdir -p {shlex.quote(PLUGIN_ROOT)} "
        f"{shlex.quote(f'{RUNTIME_BRIDGE_PLUGIN_ROOT}/runtime')} && "
        f"cat {shlex.quote(staging_file)} > {shlex.quote(HARNESS_SCRIPT)} && "
        f"cat {shlex.quote(staging_oneshot)} > {shlex.quote(ONESHOT_SCRIPT)} && "
        f"cat {shlex.quote(staging_vanilla_package)} > {shlex.quote(VANILLA_PACKAGE_SCRIPT)} && "
        f"cat {shlex.quote(staging_runtime_bridge)} > {shlex.quote(RUNTIME_BRIDGE_SERVER)} && "
        f"cat {shlex.quote(staging_pyright_setup)} > {shlex.quote(PYRIGHT_SETUP_SCRIPT)} && "
        f"chmod 755 {shlex.quote(HARNESS_SCRIPT)} && "
        f"chmod 755 {shlex.quote(ONESHOT_SCRIPT)} && "
        f"chmod 755 {shlex.quote(VANILLA_PACKAGE_SCRIPT)} && "
        f"chmod 644 {shlex.quote(RUNTIME_BRIDGE_SERVER)} && "
        f"chmod 755 {shlex.quote(PYRIGHT_SETUP_SCRIPT)} && "
        f"{bundle_install} && "
        f"rm -rf {shlex.quote(staging_dir)}",
        timeout=30,
    )
    stat = await bench.exec(
        f"test -x {shlex.quote(HARNESS_SCRIPT)} && "
        f"test -x {shlex.quote(ONESHOT_SCRIPT)} && "
        f"test -x {shlex.quote(VANILLA_PACKAGE_SCRIPT)} && "
        f"test -f {shlex.quote(RUNTIME_BRIDGE_SERVER)} && "
        f"test -f {shlex.quote(f'{BUNDLE_REMOTE_DIR}/sandbox/ephemeral_workspace/plugin/ppc_service.py')} && "
        f"test -x {shlex.quote(PYRIGHT_SETUP_SCRIPT)} && "
        f"wc -c {shlex.quote(HARNESS_SCRIPT)} "
        f"{shlex.quote(ONESHOT_SCRIPT)} "
        f"{shlex.quote(VANILLA_PACKAGE_SCRIPT)} "
        f"{shlex.quote(RUNTIME_BRIDGE_SERVER)} "
        f"{shlex.quote(PYRIGHT_SETUP_SCRIPT)}",
        timeout=30,
    )
    return {
        "path": HARNESS_SCRIPT,
        "oneshot_path": ONESHOT_SCRIPT,
        "vanilla_package_path": VANILLA_PACKAGE_SCRIPT,
        "pyright_setup_path": PYRIGHT_SETUP_SCRIPT,
        "bytes": {
            "harness": len(harness_payload),
            "oneshot": len(oneshot_payload),
            "vanilla_package": len(vanilla_package_payload),
            "runtime_bridge": len(runtime_bridge_payload),
            "pyright_setup": len(pyright_setup_payload),
            "ppc_service_bundle": ppc_service_bundle_bytes,
        },
        "mkdir": result_block(mkdir),
        "finalize": result_block(finalize),
        "stat": result_block(stat),
        "gate_pass": (
            getattr(finalize, "exit_code", 1) == 0
            and getattr(stat, "exit_code", 1) == 0
        ),
    }


async def cleanup_experiment_files(bench: DockerBench) -> None:
    await bench.exec(
        "rm -rf "
        f"{LAYER_STACK_ROOT} "
        f"{WORKSPACE_ROOT} "
        f"{PLUGIN_ROOT}/ppc "
        f"{HARNESS_SCRIPT} "
        f"{ONESHOT_SCRIPT} "
        f"{VANILLA_PACKAGE_SCRIPT} "
        f"{PYRIGHT_SETUP_SCRIPT} "
        f"{RUNTIME_BRIDGE_PLUGIN_ROOT} "
        f"{RECOVER_MARKER} "
        f"{HARNESS_LOG}",
        timeout=30,
    )


async def cleanup_processes(bench: DockerBench) -> None:
    await bench.exec(
        "pkill -f '[r]ust_ppc_harness.py' >/dev/null 2>&1 || true; "
        "pkill -f '[r]ust_vanilla_package.py' >/dev/null 2>&1 || true; "
        "pkill -f '[s]andbox.ephemeral_workspace.plugin.ppc_service' "
        ">/dev/null 2>&1 || true; "
        "pkill -f '[p]yright-langserver' >/dev/null 2>&1 || true",
        timeout=15,
    )


async def prepare_experiment_dirs(bench: DockerBench) -> dict[str, Any]:
    result = await bench.exec(
        f"mkdir -p {WORKSPACE_ROOT} {PLUGIN_ROOT}/ppc {ISOLATED_SCRATCH_ROOT}",
        timeout=30,
    )
    return {
        "workspace_root": WORKSPACE_ROOT,
        "ppc_root": f"{PLUGIN_ROOT}/ppc",
        "isolated_scratch_root": ISOLATED_SCRATCH_ROOT,
        "mkdir": result_block(result),
        "gate_pass": getattr(result, "exit_code", 1) == 0,
    }


async def configure_isolated_gate_environment(bench: DockerBench) -> dict[str, Any]:
    values = {
        "EOS_ISOLATED_WORKSPACE_ENABLED": "true",
        "EOS_ISOLATED_WORKSPACE_TEST_HARNESS": "true",
        "EOS_ISOLATED_WORKSPACE_TEST_SCRATCH_ROOT": ISOLATED_SCRATCH_ROOT,
        "EOS_ISOLATED_WORKSPACE_SETUP_TIMEOUT_S": "30",
        "EOS_ISOLATED_WORKSPACE_EXIT_GRACE_S": "0.25",
        "EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES": str(64 * 1024 * 1024),
    }
    script = ["set -e"]
    for key, value in values.items():
        script.append(f"sed -i '/^{key}=/d' /etc/environment 2>/dev/null || true")
        script.append(f"printf '%s\\n' {shlex.quote(f'{key}={value}')} >> /etc/environment")
    script.append(f"mkdir -p {shlex.quote(ISOLATED_SCRATCH_ROOT)}")
    result = await bench.exec("; ".join(script), timeout=30)
    return {
        "values": values,
        "configure": result_block(result),
        "gate_pass": getattr(result, "exit_code", 1) == 0,
    }


async def probe_isolated_plugin_gate(
    daemon_client: Any,
    sandbox_id: str,
) -> dict[str, Any]:
    report: dict[str, Any] = {}
    report["enter"] = await daemon_client.call_daemon_api(
        sandbox_id,
        "api.isolated_workspace.enter",
        {"agent_id": AGENT_ID},
        layer_stack_root=LAYER_STACK_ROOT,
        timeout=30,
    )
    try:
        report["plugin_status"] = await daemon_policy_call(
            daemon_client,
            sandbox_id,
            "api.plugin.status",
            {"agent_id": AGENT_ID},
        )
        report["plugin_dispatch"] = await daemon_policy_call(
            daemon_client,
            sandbox_id,
            "plugin.generic.ping",
            {"agent_id": AGENT_ID, "message": "blocked-in-isolated"},
        )
    finally:
        report["exit"] = await daemon_client.call_daemon_api(
            sandbox_id,
            "api.isolated_workspace.exit",
            {"agent_id": AGENT_ID, "force_cancel": True, "grace_s": 0.25},
            layer_stack_root=LAYER_STACK_ROOT,
            timeout=30,
        )
        report["status_after_exit"] = await daemon_client.call_daemon_api(
            sandbox_id,
            "api.isolated_workspace.status",
            {"agent_id": AGENT_ID},
            layer_stack_root=LAYER_STACK_ROOT,
            timeout=30,
        )
    report["gate_pass"] = (
        report.get("enter", {}).get("success") is True
        and forbidden_in_isolated_workspace(report.get("plugin_status", {}))
        and forbidden_in_isolated_workspace(report.get("plugin_dispatch", {}))
        and report.get("exit", {}).get("success") is True
        and report.get("status_after_exit", {}).get("open") is False
    )
    return report


async def daemon_policy_call(
    daemon_client: Any,
    sandbox_id: str,
    op: str,
    payload: dict[str, Any],
) -> dict[str, Any]:
    try:
        response = await daemon_client.call_daemon_api(
            sandbox_id,
            op,
            payload,
            layer_stack_root=LAYER_STACK_ROOT,
            timeout=30,
        )
    except Exception as exc:  # daemon client raises some policy denials.
        return {
            "success": False,
            "raised": True,
            "error": {
                "kind": str(getattr(exc, "kind", "")),
                "message": str(getattr(exc, "message", str(exc))),
                "details": getattr(exc, "details", {}),
            },
            "error_text": str(exc),
        }
    return {**response, "raised": False}


def forbidden_in_isolated_workspace(response: dict[str, Any]) -> bool:
    error = response.get("error")
    if isinstance(error, dict) and error.get("kind") == "forbidden_in_isolated_workspace":
        return True
    text = json.dumps(response, sort_keys=True, default=str)
    return "forbidden_in_isolated_workspace" in text


async def read_harness_log(bench: DockerBench) -> list[dict[str, Any]]:
    result = await bench.exec(f"cat {HARNESS_LOG} 2>/dev/null || true", timeout=30)
    if getattr(result, "exit_code", 1) != 0:
        return []
    events: list[dict[str, Any]] = []
    for line in str(getattr(result, "stdout", "") or "").splitlines():
        if not line.strip():
            continue
        try:
            decoded = json.loads(line)
        except json.JSONDecodeError:
            decoded = {"event": "bad_log_line", "line": line}
        events.append(decoded)
    return events


async def process_snapshot(bench: DockerBench) -> dict[str, Any]:
    result = await bench.exec(
        "pgrep -af '[r]ust_ppc_harness.py|"
        "[s]andbox.ephemeral_workspace.plugin.ppc_service' || true",
        timeout=15,
    )
    lines = [
        line
        for line in str(getattr(result, "stdout", "") or "").splitlines()
        if (
            f"python3 {HARNESS_SCRIPT}" in line
            or line.endswith(HARNESS_SCRIPT)
            or "sandbox.ephemeral_workspace.plugin.ppc_service" in line
        )
    ]
    return {"count": len(lines), "lines": lines}


def gate_pass(report: dict[str, Any]) -> bool:
    apply = report.get("apply", {})
    callback = apply.get("callback", {}) if isinstance(apply, dict) else {}
    apply_multi = report.get("apply_multi", {})
    multi_callbacks = (
        apply_multi.get("callbacks", []) if isinstance(apply_multi, dict) else []
    )
    readback = report.get("readback", {})
    runtime_bridge_ping = report.get("runtime_bridge_ping", {})
    runtime_bridge_ping_read = (
        runtime_bridge_ping.get("workspace_read", {})
        if isinstance(runtime_bridge_ping, dict)
        else {}
    )
    runtime_bridge_status = service_status(
        report.get("status_after_runtime_bridge_ping", {}),
        "runtime_bridge",
    )
    runtime_bridge_apply = report.get("runtime_bridge_apply", {})
    runtime_bridge_apply_callback = (
        runtime_bridge_apply.get("callback", {})
        if isinstance(runtime_bridge_apply, dict)
        else {}
    )
    runtime_bridge_readback = report.get("runtime_bridge_readback", {})
    runtime_bridge_concurrent = [
        item
        for item in report.get("runtime_bridge_concurrent", [])
        if isinstance(item, dict)
    ]
    runtime_bridge_concurrent_by_echo = {
        str(item.get("echo")): item for item in runtime_bridge_concurrent
    }
    runtime_bridge_slow = runtime_bridge_concurrent_by_echo.get("slow-first", {})
    runtime_bridge_fast = runtime_bridge_concurrent_by_echo.get("fast-second", {})
    runtime_bridge_concurrent_apply = [
        item
        for item in report.get("runtime_bridge_concurrent_apply", [])
        if isinstance(item, dict)
    ]
    runtime_bridge_concurrent_apply_paths = {
        str(path)
        for item in runtime_bridge_concurrent_apply
        for path in item.get("changed_paths", [])
    }
    runtime_bridge_concurrent_readback_a = report.get(
        "runtime_bridge_concurrent_readback_a",
        {},
    )
    runtime_bridge_concurrent_readback_b = report.get(
        "runtime_bridge_concurrent_readback_b",
        {},
    )
    lsp_bridge_query_symbols = report.get("lsp_bridge_query_symbols", {})
    lsp_bridge_query_symbols_lsp = (
        lsp_bridge_query_symbols.get("lsp", {})
        if isinstance(lsp_bridge_query_symbols, dict)
        else {}
    )
    lsp_bridge_find_definitions = report.get("lsp_bridge_find_definitions", {})
    lsp_bridge_find_definitions_lsp = (
        lsp_bridge_find_definitions.get("lsp", {})
        if isinstance(lsp_bridge_find_definitions, dict)
        else {}
    )
    lsp_bridge_find_references = report.get("lsp_bridge_find_references", {})
    lsp_bridge_find_references_lsp = (
        lsp_bridge_find_references.get("lsp", {})
        if isinstance(lsp_bridge_find_references, dict)
        else {}
    )
    lsp_bridge_signature_help = report.get("lsp_bridge_signature_help", {})
    lsp_bridge_signature_help_lsp = (
        lsp_bridge_signature_help.get("lsp", {})
        if isinstance(lsp_bridge_signature_help, dict)
        else {}
    )
    lsp_bridge_signature_labels = lsp_bridge_signature_help_lsp.get("labels", [])
    lsp_bridge_document_highlight = report.get("lsp_bridge_document_highlight", {})
    lsp_bridge_document_highlight_lsp = (
        lsp_bridge_document_highlight.get("lsp", {})
        if isinstance(lsp_bridge_document_highlight, dict)
        else {}
    )
    lsp_bridge_highlight_start_lines = set(
        lsp_bridge_document_highlight_lsp.get("highlight_start_lines", [])
    )
    lsp_bridge_diagnostics = report.get("lsp_bridge_diagnostics", {})
    lsp_bridge_diagnostics_lsp = (
        lsp_bridge_diagnostics.get("lsp", {})
        if isinstance(lsp_bridge_diagnostics, dict)
        else {}
    )
    lsp_bridge_diagnostic_messages = lsp_bridge_diagnostics_lsp.get(
        "diagnostic_messages",
        [],
    )
    lsp_bridge_code_actions = report.get("lsp_bridge_code_actions", {})
    lsp_bridge_code_actions_lsp = (
        lsp_bridge_code_actions.get("lsp", {})
        if isinstance(lsp_bridge_code_actions, dict)
        else {}
    )
    lsp_bridge_hover = report.get("lsp_bridge_hover", {})
    lsp_bridge_hover_lsp = (
        lsp_bridge_hover.get("lsp", {}) if isinstance(lsp_bridge_hover, dict) else {}
    )
    lsp_bridge_rename = report.get("lsp_bridge_rename", {})
    lsp_bridge_rename_lsp = (
        lsp_bridge_rename.get("lsp", {})
        if isinstance(lsp_bridge_rename, dict)
        else {}
    )
    lsp_bridge_rename_apply = (
        lsp_bridge_rename_lsp.get("apply", {})
        if isinstance(lsp_bridge_rename_lsp, dict)
        else {}
    )
    lsp_bridge_rename_readback = report.get("lsp_bridge_rename_readback", {})
    lsp_bridge_apply = report.get("lsp_bridge_apply_workspace_edit", {})
    lsp_bridge_apply_lsp = (
        lsp_bridge_apply.get("lsp", {}) if isinstance(lsp_bridge_apply, dict) else {}
    )
    lsp_bridge_apply_result = (
        lsp_bridge_apply_lsp.get("apply", {})
        if isinstance(lsp_bridge_apply_lsp, dict)
        else {}
    )
    lsp_bridge_apply_readback = report.get("lsp_bridge_apply_readback", {})
    lsp_bridge_code_action = report.get("lsp_bridge_apply_code_action", {})
    lsp_bridge_code_action_lsp = (
        lsp_bridge_code_action.get("lsp", {})
        if isinstance(lsp_bridge_code_action, dict)
        else {}
    )
    lsp_bridge_code_action_apply = (
        lsp_bridge_code_action_lsp.get("apply", {})
        if isinstance(lsp_bridge_code_action_lsp, dict)
        else {}
    )
    lsp_bridge_code_action_readback = report.get(
        "lsp_bridge_code_action_readback",
        {},
    )
    lsp_bridge_format = report.get("lsp_bridge_format_document", {})
    lsp_bridge_format_lsp = (
        lsp_bridge_format.get("lsp", {})
        if isinstance(lsp_bridge_format, dict)
        else {}
    )
    lsp_bridge_format_apply = (
        lsp_bridge_format_lsp.get("apply", {})
        if isinstance(lsp_bridge_format_lsp, dict)
        else {}
    )
    lsp_bridge_format_readback = report.get("lsp_bridge_format_readback", {})
    lsp_bridge_execute_command = report.get("lsp_bridge_execute_command", {})
    lsp_bridge_execute_command_lsp = (
        lsp_bridge_execute_command.get("lsp", {})
        if isinstance(lsp_bridge_execute_command, dict)
        else {}
    )
    lsp_bridge_execute_command_apply = (
        lsp_bridge_execute_command_lsp.get("apply", {})
        if isinstance(lsp_bridge_execute_command_lsp, dict)
        else {}
    )
    lsp_bridge_execute_command_readback = report.get(
        "lsp_bridge_execute_command_readback",
        {},
    )
    multi_readback_a = report.get("multi_readback_a", {})
    multi_readback_b = report.get("multi_readback_b", {})
    shell_publish = report.get("shell_publish", {})
    shell_readback = report.get("shell_readback", {})
    shell_refresh_ping = report.get("shell_refresh_ping", {})
    concurrent_ping = [
        item for item in report.get("concurrent_ping", []) if isinstance(item, dict)
    ]
    concurrent_echoes = {str(item.get("echo")) for item in concurrent_ping}
    concurrent_manifest_keys = {
        str(item.get("manifest_key"))
        for item in concurrent_ping
        if item.get("manifest_key")
    }
    refresh_status = service_status(report.get("status_after_refresh", {}), "harness")
    adapter_status = service_status(report.get("status_after_adapter", {}), "adapter_harness")
    pyright_status = service_status(report.get("status_after_pyright", {}), "pyright_harness")
    restart_status = service_status(report.get("status_after_restart", {}), "restart_harness")
    crash_status = service_status(report.get("status_after_crash", {}), "crash_harness")
    crash_recover_status = service_status(
        report.get("status_after_crash_recover", {}),
        "crash_harness",
    )
    hang_status = service_status(report.get("status_after_hang", {}), "hang_harness")
    hang_recover_status = service_status(
        report.get("status_after_hang_recover", {}),
        "hang_harness",
    )
    recover_failed_status = service_status(
        report.get("status_after_recover_failure", {}),
        "recover_harness",
    )
    recover_status = service_status(report.get("status_after_recover", {}), "recover_harness")
    health_probe = {
        str(item.get("service_id")): item
        for item in report.get("status_after_health_probe", {}).get("service_health", [])
        if isinstance(item, dict)
    }
    shell_refresh_status = service_status(
        report.get("status_after_shell_refresh", {}),
        "harness",
    )
    health_fail_probe = health_probe.get("health_fail_harness", {})
    health_fail_status = service_status(
        report.get("status_after_health_probe", {}),
        "health_fail_harness",
    )
    health_fail_recover_status = service_status(
        report.get("status_after_health_fail_recover", {}),
        "health_fail_harness",
    )
    health_fail_recover_ping = report.get("health_fail_recover_ping", {})
    expected_health_services = {
        "harness",
        "restart_harness",
        "adapter_harness",
        "runtime_bridge",
        "pyright_harness",
        "crash_harness",
        "hang_harness",
        "recover_harness",
    }
    oneshot = report.get("oneshot", {})
    oneshot_readback = report.get("oneshot_readback", {})
    final_metrics = report.get("final_metrics", {})
    post_cleanup_metrics = report.get("post_cleanup_metrics", {})
    status_after_cleanup = report.get("status_after_cleanup", {})
    processes_before_cleanup = report.get("processes_before_cleanup", {})
    processes_after_cleanup = report.get("processes_after_cleanup", {})
    isolated_plugin_gate = report.get("isolated_plugin_gate", {})
    adapter_query = report.get("adapter_query", {})
    adapter_package = adapter_query.get("package", {}) if isinstance(adapter_query, dict) else {}
    co_shared_refresh = report.get("co_shared_refresh", {})
    pyright_symbols = report.get("pyright_symbols", {})
    pyright_lsp = (
        pyright_symbols.get("lsp", {}) if isinstance(pyright_symbols, dict) else {}
    )
    pyright_workspace_symbols = report.get("pyright_workspace_symbols", {})
    pyright_workspace_symbols_lsp = (
        pyright_workspace_symbols.get("lsp", {})
        if isinstance(pyright_workspace_symbols, dict)
        else {}
    )
    pyright_capabilities = report.get("pyright_capabilities", {})
    pyright_capabilities_lsp = (
        pyright_capabilities.get("lsp", {})
        if isinstance(pyright_capabilities, dict)
        else {}
    )
    pyright_capability_supports = pyright_capabilities_lsp.get("supports", {})
    pyright_code_action_provider = (
        pyright_capabilities_lsp.get("raw", {}).get("codeActionProvider", {})
        if isinstance(pyright_capabilities_lsp.get("raw"), dict)
        else {}
    )
    pyright_code_action_provider_kinds = (
        pyright_code_action_provider.get("codeActionKinds", [])
        if isinstance(pyright_code_action_provider, dict)
        else []
    )
    pyright_document_formatting = report.get("pyright_document_formatting", {})
    pyright_document_formatting_lsp = (
        pyright_document_formatting.get("lsp", {})
        if isinstance(pyright_document_formatting, dict)
        else {}
    )
    pyright_execute_command = report.get("pyright_execute_command", {})
    pyright_execute_command_lsp = (
        pyright_execute_command.get("lsp", {})
        if isinstance(pyright_execute_command, dict)
        else {}
    )
    pyright_completion = report.get("pyright_completion", {})
    pyright_completion_lsp = (
        pyright_completion.get("lsp", {}) if isinstance(pyright_completion, dict) else {}
    )
    pyright_completion_resolve = report.get("pyright_completion_resolve", {})
    pyright_completion_resolve_lsp = (
        pyright_completion_resolve.get("lsp", {})
        if isinstance(pyright_completion_resolve, dict)
        else {}
    )
    pyright_diagnostics = report.get("pyright_diagnostics", {})
    pyright_diagnostics_lsp = (
        pyright_diagnostics.get("lsp", {})
        if isinstance(pyright_diagnostics, dict)
        else {}
    )
    pyright_diagnostic_messages = pyright_diagnostics_lsp.get(
        "diagnostic_messages",
        [],
    )
    pyright_code_actions = report.get("pyright_code_actions", {})
    pyright_code_actions_lsp = (
        pyright_code_actions.get("lsp", {})
        if isinstance(pyright_code_actions, dict)
        else {}
    )
    pyright_code_action_kinds = pyright_code_actions_lsp.get("action_kinds", [])
    pyright_signature_help = report.get("pyright_signature_help", {})
    pyright_signature_help_lsp = (
        pyright_signature_help.get("lsp", {})
        if isinstance(pyright_signature_help, dict)
        else {}
    )
    pyright_signature_labels = pyright_signature_help_lsp.get("labels", [])
    pyright_hover = report.get("pyright_hover", {})
    pyright_hover_lsp = (
        pyright_hover.get("lsp", {}) if isinstance(pyright_hover, dict) else {}
    )
    pyright_hover_text = str(pyright_hover_lsp.get("hover_text", ""))
    pyright_type_definition = report.get("pyright_type_definition", {})
    pyright_type_definition_lsp = (
        pyright_type_definition.get("lsp", {})
        if isinstance(pyright_type_definition, dict)
        else {}
    )
    pyright_type_definition_locations = pyright_type_definition_lsp.get("locations", [])
    pyright_declaration = report.get("pyright_declaration", {})
    pyright_declaration_lsp = (
        pyright_declaration.get("lsp", {})
        if isinstance(pyright_declaration, dict)
        else {}
    )
    pyright_declaration_locations = pyright_declaration_lsp.get("locations", [])
    pyright_call_hierarchy = report.get("pyright_call_hierarchy", {})
    pyright_call_hierarchy_lsp = (
        pyright_call_hierarchy.get("lsp", {})
        if isinstance(pyright_call_hierarchy, dict)
        else {}
    )
    pyright_call_hierarchy_outgoing = report.get("pyright_call_hierarchy_outgoing", {})
    pyright_call_hierarchy_outgoing_lsp = (
        pyright_call_hierarchy_outgoing.get("lsp", {})
        if isinstance(pyright_call_hierarchy_outgoing, dict)
        else {}
    )
    pyright_document_highlight = report.get("pyright_document_highlight", {})
    pyright_document_highlight_lsp = (
        pyright_document_highlight.get("lsp", {})
        if isinstance(pyright_document_highlight, dict)
        else {}
    )
    pyright_highlights = pyright_document_highlight_lsp.get("highlights", [])
    pyright_highlight_start_lines = {
        highlight.get("range", {}).get("start", {}).get("line")
        for highlight in pyright_highlights
        if isinstance(highlight, dict) and highlight.get("path") == PYRIGHT_TARGET_REL
    }
    pyright_prepare_rename = report.get("pyright_prepare_rename", {})
    pyright_prepare_rename_lsp = (
        pyright_prepare_rename.get("lsp", {})
        if isinstance(pyright_prepare_rename, dict)
        else {}
    )
    pyright_prepare_rename_range = pyright_prepare_rename_lsp.get("range", {})
    pyright_definition = report.get("pyright_definition", {})
    pyright_definition_lsp = (
        pyright_definition.get("lsp", {}) if isinstance(pyright_definition, dict) else {}
    )
    pyright_definition_locations = pyright_definition_lsp.get("locations", [])
    pyright_references = report.get("pyright_references", {})
    pyright_references_lsp = (
        pyright_references.get("lsp", {}) if isinstance(pyright_references, dict) else {}
    )
    pyright_reference_locations = pyright_references_lsp.get("locations", [])
    pyright_reference_start_lines = {
        location.get("range", {}).get("start", {}).get("line")
        for location in pyright_reference_locations
        if isinstance(location, dict) and location.get("path") == PYRIGHT_TARGET_REL
    }
    lsp_apply_workspace_edit = report.get("lsp_apply_workspace_edit", {})
    lsp_apply_workspace_edit_callback = (
        lsp_apply_workspace_edit.get("callback", {})
        if isinstance(lsp_apply_workspace_edit, dict)
        else {}
    )
    lsp_apply_workspace_edit_readback = report.get(
        "lsp_apply_workspace_edit_readback",
        {},
    )
    lsp_apply_code_action = report.get("lsp_apply_code_action", {})
    lsp_apply_code_action_callback = (
        lsp_apply_code_action.get("callback", {})
        if isinstance(lsp_apply_code_action, dict)
        else {}
    )
    lsp_apply_code_action_readback = report.get(
        "lsp_apply_code_action_readback",
        {},
    )
    lsp_format_document = report.get("lsp_format_document", {})
    lsp_format_document_callback = (
        lsp_format_document.get("callback", {})
        if isinstance(lsp_format_document, dict)
        else {}
    )
    lsp_format_readback = report.get("lsp_format_readback", {})
    lsp_execute_command = report.get("lsp_execute_command", {})
    lsp_execute_command_callback = (
        lsp_execute_command.get("callback", {})
        if isinstance(lsp_execute_command, dict)
        else {}
    )
    lsp_execute_command_readback = report.get(
        "lsp_execute_command_readback",
        {},
    )
    pyright_rename = report.get("pyright_rename", {})
    pyright_rename_lsp = (
        pyright_rename.get("lsp", {}) if isinstance(pyright_rename, dict) else {}
    )
    pyright_rename_callback = (
        pyright_rename.get("callback", {}) if isinstance(pyright_rename, dict) else {}
    )
    pyright_rename_readback = report.get("pyright_rename_readback", {})
    return bool(
        report.get("artifact", {}).get("gate_pass")
        and report.get("harness", {}).get("gate_pass")
        and report.get("experiment_dirs", {}).get("gate_pass")
        and report.get("pyright_setup", {}).get("exit_code") == 0
        and report.get("ready", {}).get("ready") is True
        and report.get("ensure", {}).get("success") is True
        and report.get("ensure", {}).get("service_processes_started") is True
        and "plugin.generic.ping"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.restart_ping"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.adapter_query"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.runtime_bridge_ping"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.runtime_bridge_apply"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.runtime_bridge_delay_ping"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_query_symbols"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_find_definitions"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_find_references"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_signature_help"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_document_highlight"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_diagnostics"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_code_actions"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_hover"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_rename"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_apply_workspace_edit"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_apply_code_action"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_format_document"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_bridge_execute_command"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_symbols"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_workspace_symbols"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_capabilities"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_document_formatting"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_execute_command"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_completion"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_completion_resolve"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_diagnostics"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_code_actions"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_signature_help"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_hover"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_type_definition"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_declaration"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_call_hierarchy"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_document_highlight"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_prepare_rename"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_definition"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_references"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.pyright_rename"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_apply_workspace_edit"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_apply_code_action"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_format_document"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.lsp_execute_command"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.crash_probe"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.crash_recover_ping"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.hang_probe"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.hang_recover_ping"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.recover_probe"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.health_fail_ping"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.health_fail_recover_ping"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and "plugin.generic.apply_multi"
        in report.get("status_after_ensure", {}).get("connected_ppc_routes", [])
        and expected_health_services.issubset(health_probe)
        and all(
            health_probe[service_id].get("success") is True
            for service_id in expected_health_services
        )
        and health_fail_probe.get("success") is False
        and "intentional health failure" in str(health_fail_probe.get("error", ""))
        and "plugin.generic.health_fail_ping"
        not in report.get("status_after_health_probe", {}).get("connected_ppc_routes", [])
        and "plugin.generic.health_fail_recover_ping"
        not in report.get("status_after_health_probe", {}).get("connected_ppc_routes", [])
        and health_fail_status.get("state") == "stopped"
        and health_fail_recover_ping.get("from_health_recovered_service") is True
        and health_fail_recover_ping.get("from_ppc") is True
        and health_fail_recover_ping.get("workspace_mounted") is True
        and health_fail_recover_ping.get("echo") == "after-health-fail-recover"
        and "plugin.generic.health_fail_recover_ping"
        in report.get("status_after_health_fail_recover", {}).get(
            "connected_ppc_routes",
            [],
        )
        and health_fail_recover_status.get("state") == "ready"
        and int(health_fail_recover_status.get("restart_count", 0)) >= 1
        and report.get("ping", {}).get("from_ppc") is True
        and report.get("ping", {}).get("workspace_mounted") is True
        and len(concurrent_ping) == 2
        and concurrent_echoes == {"concurrent-a", "concurrent-b"}
        and len(concurrent_manifest_keys) == 1
        and report.get("ping", {}).get("manifest_key") in concurrent_manifest_keys
        and all(
            item.get("success") is True
            and item.get("from_ppc") is True
            and item.get("workspace_mounted") is True
            and item.get("service_id") == "harness"
            for item in concurrent_ping
        )
        and apply.get("from_self_managed") is True
        and callback.get("success") is True
        and readback.get("exists") is True
        and readback.get("content") == TARGET_CONTENT
        and runtime_bridge_ping.get("from_runtime_bridge") is True
        and runtime_bridge_ping.get("from_ppc_service_bridge") is True
        and runtime_bridge_ping.get("workspace_mounted") is True
        and runtime_bridge_ping_read.get("content") == TARGET_CONTENT
        and runtime_bridge_status.get("state") == "ready"
        and int(runtime_bridge_status.get("refresh_count", 0)) >= 1
        and runtime_bridge_apply.get("from_runtime_bridge") is True
        and runtime_bridge_apply.get("from_ppc_service_bridge") is True
        and runtime_bridge_apply.get("from_mounted_workspace_callback") is True
        and runtime_bridge_apply.get("workspace_mounted") is True
        and runtime_bridge_apply_callback.get("success") is True
        and RUNTIME_BRIDGE_TARGET_REL
        in runtime_bridge_apply.get("changed_paths", [])
        and runtime_bridge_readback.get("exists") is True
        and runtime_bridge_readback.get("content") == RUNTIME_BRIDGE_CONTENT
        and len(runtime_bridge_concurrent) == 2
        and runtime_bridge_slow.get("from_runtime_bridge") is True
        and runtime_bridge_fast.get("from_runtime_bridge") is True
        and runtime_bridge_slow.get("from_ppc_service_bridge") is True
        and runtime_bridge_fast.get("from_ppc_service_bridge") is True
        and runtime_bridge_slow.get("workspace_mounted") is True
        and runtime_bridge_fast.get("workspace_mounted") is True
        and float(runtime_bridge_slow.get("delay_s", 0)) >= 0.3
        and float(runtime_bridge_fast.get("delay_s", 1)) == 0.0
        and float(runtime_bridge_fast.get("service_finished_at_s", 0))
        < float(runtime_bridge_slow.get("service_finished_at_s", 0))
        and float(runtime_bridge_fast.get("client_elapsed_s", 99))
        < float(runtime_bridge_slow.get("client_elapsed_s", 0))
        and len(runtime_bridge_concurrent_apply) == 2
        and all(
            item.get("from_runtime_bridge") is True
            and item.get("from_ppc_service_bridge") is True
            and item.get("from_mounted_workspace_callback") is True
            and item.get("workspace_mounted") is True
            and item.get("callback", {}).get("success") is True
            for item in runtime_bridge_concurrent_apply
        )
        and runtime_bridge_concurrent_apply_paths
        == {RUNTIME_BRIDGE_CONCURRENT_A_REL, RUNTIME_BRIDGE_CONCURRENT_B_REL}
        and runtime_bridge_concurrent_readback_a.get("content")
        == RUNTIME_BRIDGE_CONCURRENT_A_CONTENT
        and runtime_bridge_concurrent_readback_b.get("content")
        == RUNTIME_BRIDGE_CONCURRENT_B_CONTENT
        and report.get("lsp_bridge_seed", {}).get("success") is True
        and lsp_bridge_query_symbols.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_query_symbols.get("from_ppc_service_bridge") is True
        and lsp_bridge_query_symbols.get("workspace_mounted") is True
        and lsp_bridge_query_symbols_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_query_symbols_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and LSP_BRIDGE_RENAMED_SYMBOL
        in lsp_bridge_query_symbols_lsp.get("symbol_names", [])
        and lsp_bridge_find_definitions.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_find_definitions.get("from_ppc_service_bridge") is True
        and lsp_bridge_find_definitions.get("workspace_mounted") is True
        and lsp_bridge_find_definitions_lsp.get("protocol")
        == "lsp-python-importlib"
        and lsp_bridge_find_definitions_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and int(lsp_bridge_find_definitions_lsp.get("definition_count", 0)) >= 1
        and LSP_BRIDGE_TARGET_REL
        in lsp_bridge_find_definitions_lsp.get("definition_paths", [])
        and 0 in lsp_bridge_find_definitions_lsp.get("definition_start_lines", [])
        and lsp_bridge_find_references.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_find_references.get("from_ppc_service_bridge") is True
        and lsp_bridge_find_references.get("workspace_mounted") is True
        and lsp_bridge_find_references_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_find_references_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and int(lsp_bridge_find_references_lsp.get("reference_count", 0)) >= 2
        and LSP_BRIDGE_TARGET_REL
        in lsp_bridge_find_references_lsp.get("reference_paths", [])
        and {0, 3}.issubset(
            set(lsp_bridge_find_references_lsp.get("reference_start_lines", []))
        )
        and lsp_bridge_signature_help.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_signature_help.get("from_ppc_service_bridge") is True
        and lsp_bridge_signature_help.get("workspace_mounted") is True
        and lsp_bridge_signature_help_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_signature_help_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and lsp_bridge_signature_help_lsp.get("path") == PYRIGHT_SIGNATURE_TARGET_REL
        and lsp_bridge_signature_help_lsp.get("position", {}).get("line")
        == PYRIGHT_SIGNATURE_LINE
        and lsp_bridge_signature_help_lsp.get("position", {}).get("character")
        == PYRIGHT_SIGNATURE_CHARACTER
        and int(lsp_bridge_signature_help_lsp.get("signature_count", 0)) >= 1
        and lsp_bridge_signature_help_lsp.get("active_parameter") == 1
        and any(
            isinstance(label, str) and "left: int" in label and "right: str" in label
            for label in lsp_bridge_signature_labels
        )
        and lsp_bridge_document_highlight.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_document_highlight.get("from_ppc_service_bridge") is True
        and lsp_bridge_document_highlight.get("workspace_mounted") is True
        and lsp_bridge_document_highlight_lsp.get("protocol")
        == "lsp-python-importlib"
        and lsp_bridge_document_highlight_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and lsp_bridge_document_highlight_lsp.get("path") == PYRIGHT_TARGET_REL
        and int(lsp_bridge_document_highlight_lsp.get("highlight_count", 0)) >= 2
        and {0, 3}.issubset(lsp_bridge_highlight_start_lines)
        and report.get("lsp_bridge_diagnostics_seed", {}).get("success") is True
        and lsp_bridge_diagnostics.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_diagnostics.get("from_ppc_service_bridge") is True
        and lsp_bridge_diagnostics.get("workspace_mounted") is True
        and lsp_bridge_diagnostics_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_diagnostics_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and lsp_bridge_diagnostics_lsp.get("path")
        == LSP_BRIDGE_DIAGNOSTICS_TARGET_REL
        and lsp_bridge_diagnostics_lsp.get("position", {}).get("line")
        == LSP_BRIDGE_DIAGNOSTICS_LINE
        and lsp_bridge_diagnostics_lsp.get("position", {}).get("character")
        == LSP_BRIDGE_DIAGNOSTICS_CHARACTER
        and lsp_bridge_diagnostics_lsp.get("wait_for_diagnostics") is True
        and int(lsp_bridge_diagnostics_lsp.get("diagnostic_count", 0)) >= 1
        and any(
            isinstance(message, str) and LSP_BRIDGE_DIAGNOSTICS_SYMBOL in message
            for message in lsp_bridge_diagnostic_messages
        )
        and lsp_bridge_code_actions.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_code_actions.get("from_ppc_service_bridge") is True
        and lsp_bridge_code_actions.get("workspace_mounted") is True
        and lsp_bridge_code_actions_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_code_actions_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and lsp_bridge_code_actions_lsp.get("path")
        == LSP_BRIDGE_DIAGNOSTICS_TARGET_REL
        and lsp_bridge_code_actions_lsp.get("position", {}).get("line")
        == LSP_BRIDGE_DIAGNOSTICS_LINE
        and lsp_bridge_code_actions_lsp.get("position", {}).get("character")
        == LSP_BRIDGE_DIAGNOSTICS_CHARACTER
        and LSP_BRIDGE_CODE_ACTION_KIND in lsp_bridge_code_actions_lsp.get("only", [])
        and isinstance(lsp_bridge_code_actions_lsp.get("actions"), list)
        and int(lsp_bridge_code_actions_lsp.get("action_count", -1)) >= 0
        and lsp_bridge_hover.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_hover.get("from_ppc_service_bridge") is True
        and lsp_bridge_hover.get("workspace_mounted") is True
        and lsp_bridge_hover_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_hover_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and LSP_BRIDGE_RENAMED_SYMBOL
        in str(lsp_bridge_hover_lsp.get("hover_text", ""))
        and lsp_bridge_rename.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_rename.get("from_ppc_service_bridge") is True
        and lsp_bridge_rename.get("from_mounted_workspace_callback") is True
        and lsp_bridge_rename.get("workspace_mounted") is True
        and lsp_bridge_rename_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_rename_lsp.get("new_name") == LSP_BRIDGE_RENAMED_SYMBOL
        and lsp_bridge_rename_apply.get("success") is True
        and LSP_BRIDGE_TARGET_REL in lsp_bridge_rename.get("changed_paths", [])
        and lsp_bridge_rename_readback.get("exists") is True
        and lsp_bridge_rename_readback.get("content") == LSP_BRIDGE_RENAMED_CONTENT
        and report.get("lsp_bridge_apply_seed", {}).get("success") is True
        and lsp_bridge_apply.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_apply.get("from_ppc_service_bridge") is True
        and lsp_bridge_apply.get("from_mounted_workspace_callback") is True
        and lsp_bridge_apply.get("workspace_mounted") is True
        and lsp_bridge_apply_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_apply_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and lsp_bridge_apply_result.get("success") is True
        and LSP_BRIDGE_APPLY_TARGET_REL in lsp_bridge_apply.get("changed_paths", [])
        and lsp_bridge_apply_readback.get("exists") is True
        and lsp_bridge_apply_readback.get("content") == LSP_BRIDGE_APPLY_CONTENT_AFTER
        and report.get("lsp_bridge_code_action_seed", {}).get("success") is True
        and lsp_bridge_code_action.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_code_action.get("from_ppc_service_bridge") is True
        and lsp_bridge_code_action.get("from_mounted_workspace_callback") is True
        and lsp_bridge_code_action.get("workspace_mounted") is True
        and lsp_bridge_code_action_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_code_action_lsp.get("server")
        == "plugins.catalog.lsp.runtime.server"
        and lsp_bridge_code_action_lsp.get("action_title")
        == LSP_BRIDGE_CODE_ACTION_TITLE
        and lsp_bridge_code_action_lsp.get("action_kind")
        == LSP_BRIDGE_CODE_ACTION_KIND
        and lsp_bridge_code_action_apply.get("success") is True
        and LSP_BRIDGE_CODE_ACTION_TARGET_REL
        in lsp_bridge_code_action.get("changed_paths", [])
        and lsp_bridge_code_action_readback.get("exists") is True
        and lsp_bridge_code_action_readback.get("content")
        == LSP_BRIDGE_CODE_ACTION_CONTENT_AFTER
        and report.get("lsp_bridge_format_seed", {}).get("success") is True
        and lsp_bridge_format.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_format.get("from_ppc_service_bridge") is True
        and lsp_bridge_format.get("from_mounted_workspace_callback") is True
        and lsp_bridge_format.get("workspace_mounted") is True
        and lsp_bridge_format_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_format_lsp.get("server")
        == "plugins.catalog.lsp.runtime.apply"
        and lsp_bridge_format_lsp.get("method") == "textDocument/formatting"
        and int(lsp_bridge_format_lsp.get("edit_count", 0)) >= 1
        and lsp_bridge_format_apply.get("success") is True
        and LSP_BRIDGE_FORMAT_TARGET_REL in lsp_bridge_format.get("changed_paths", [])
        and lsp_bridge_format_readback.get("exists") is True
        and lsp_bridge_format_readback.get("content")
        == LSP_BRIDGE_FORMAT_CONTENT_AFTER
        and report.get("lsp_bridge_execute_command_seed", {}).get("success") is True
        and lsp_bridge_execute_command.get("from_lsp_importlib_bridge") is True
        and lsp_bridge_execute_command.get("from_ppc_service_bridge") is True
        and lsp_bridge_execute_command.get("from_mounted_workspace_callback") is True
        and lsp_bridge_execute_command.get("workspace_mounted") is True
        and lsp_bridge_execute_command_lsp.get("protocol") == "lsp-python-importlib"
        and lsp_bridge_execute_command_lsp.get("server")
        == "plugins.catalog.lsp.runtime.apply"
        and lsp_bridge_execute_command_lsp.get("method") == "workspace/executeCommand"
        and lsp_bridge_execute_command_lsp.get("command")
        == LSP_BRIDGE_EXECUTE_COMMAND_NAME
        and lsp_bridge_execute_command_lsp.get("supported") is True
        and lsp_bridge_execute_command_lsp.get("unsupported") is False
        and lsp_bridge_execute_command_apply.get("success") is True
        and LSP_BRIDGE_EXECUTE_COMMAND_TARGET_REL
        in lsp_bridge_execute_command.get("changed_paths", [])
        and lsp_bridge_execute_command_readback.get("exists") is True
        and lsp_bridge_execute_command_readback.get("content")
        == LSP_BRIDGE_EXECUTE_COMMAND_CONTENT_AFTER
        and apply_multi.get("from_self_managed") is True
        and apply_multi.get("callback_count") == 2
        and len(multi_callbacks) == 2
        and all(
            isinstance(item, dict) and item.get("success") is True
            for item in multi_callbacks
        )
        and {MULTI_TARGET_A_REL, MULTI_TARGET_B_REL}.issubset(
            set(apply_multi.get("changed_paths", []))
        )
        and multi_readback_a.get("exists") is True
        and multi_readback_a.get("content") == MULTI_TARGET_A_CONTENT
        and multi_readback_b.get("exists") is True
        and multi_readback_b.get("content") == MULTI_TARGET_B_CONTENT
        and shell_publish.get("exit_code") == 0
        and shell_publish.get("status") in {"ok", "committed"}
        and shell_readback.get("exists") is True
        and shell_readback.get("content") == SHELL_CONTENT
        and shell_refresh_ping.get("from_ppc") is True
        and shell_refresh_ping.get("workspace_mounted") is True
        and shell_refresh_ping.get("workspace_read", {}).get("content")
        == SHELL_CONTENT
        and shell_refresh_status.get("state") == "ready"
        and int(shell_refresh_status.get("refresh_count", 0)) >= 1
        and report.get("refresh_ping", {}).get("from_ppc") is True
        and report.get("refresh_ping", {}).get("workspace_mounted") is True
        and report.get("refresh_ping", {}).get("workspace_read", {}).get("content")
        == TARGET_CONTENT
        and refresh_status.get("state") == "ready"
        and int(refresh_status.get("refresh_count", 0)) >= 1
        and adapter_query.get("from_package_adapter") is True
        and adapter_query.get("workspace_mounted") is True
        and adapter_package.get("protocol") == "line-json-v1"
        and adapter_package.get("cached") is True
        and adapter_package.get("content") == TARGET_CONTENT
        and adapter_status.get("state") == "ready"
        and int(adapter_status.get("refresh_count", 0)) >= 1
        and co_shared_refresh.get("same_manifest_key") is True
        and co_shared_refresh.get("first_service_id") == "harness"
        and co_shared_refresh.get("second_service_id") == "adapter_harness"
        and co_shared_refresh.get("first_state") == "ready"
        and co_shared_refresh.get("second_state") == "ready"
        and int(co_shared_refresh.get("first_refresh_count", 0)) >= 1
        and int(co_shared_refresh.get("second_refresh_count", 0)) >= 1
        and int(co_shared_refresh.get("first_restart_count", -1)) == 0
        and int(co_shared_refresh.get("second_restart_count", -1)) == 0
        and report.get("pyright_seed", {}).get("success") is True
        and report.get("pyright_completion_seed", {}).get("success") is True
        and report.get("pyright_diagnostics_seed", {}).get("success") is True
        and report.get("pyright_code_action_seed", {}).get("success") is True
        and report.get("lsp_apply_workspace_edit_seed", {}).get("success") is True
        and report.get("lsp_apply_code_action_seed", {}).get("success") is True
        and report.get("lsp_format_seed", {}).get("success") is True
        and report.get("lsp_execute_command_seed", {}).get("success") is True
        and report.get("pyright_signature_seed", {}).get("success") is True
        and report.get("pyright_type_seed", {}).get("success") is True
        and report.get("pyright_call_hierarchy_seed", {}).get("success") is True
        and pyright_symbols.get("from_pyright_adapter") is True
        and pyright_symbols.get("workspace_mounted") is True
        and pyright_lsp.get("protocol") == "lsp-jsonrpc"
        and PYRIGHT_SYMBOL in pyright_lsp.get("symbol_names", [])
        and pyright_workspace_symbols.get("from_pyright_adapter") is True
        and pyright_workspace_symbols.get("workspace_mounted") is True
        and pyright_workspace_symbols_lsp.get("protocol") == "lsp-jsonrpc"
        and int(pyright_workspace_symbols_lsp.get("symbol_count", 0)) >= 1
        and PYRIGHT_SYMBOL in pyright_workspace_symbols_lsp.get("symbol_names", [])
        and PYRIGHT_TARGET_REL in pyright_workspace_symbols_lsp.get("symbol_paths", [])
        and pyright_capabilities.get("from_pyright_adapter") is True
        and pyright_capabilities.get("workspace_mounted") is True
        and pyright_capabilities_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_capability_supports.get("completion") is True
        and pyright_capability_supports.get("hover") is True
        and pyright_capability_supports.get("signature_help") is True
        and pyright_capability_supports.get("definition") is True
        and pyright_capability_supports.get("declaration") is True
        and pyright_capability_supports.get("type_definition") is True
        and pyright_capability_supports.get("document_highlight") is True
        and pyright_capability_supports.get("document_symbol") is True
        and pyright_capability_supports.get("workspace_symbol") is True
        and pyright_capability_supports.get("references") is True
        and pyright_capability_supports.get("rename") is True
        and pyright_capability_supports.get("code_action") is True
        and pyright_capability_supports.get("document_formatting") is False
        and pyright_capability_supports.get("document_range_formatting") is False
        and pyright_capability_supports.get("execute_command_provider") is True
        and pyright_capability_supports.get("execute_command") is False
        and PYRIGHT_CODE_ACTION_KIND in pyright_code_action_provider_kinds
        and pyright_capability_supports.get("call_hierarchy") is True
        and pyright_capability_supports.get("completion_resolve") is True
        and pyright_document_formatting.get("from_pyright_adapter") is True
        and pyright_document_formatting.get("workspace_mounted") is True
        and pyright_document_formatting.get("success") is False
        and pyright_document_formatting_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_document_formatting_lsp.get("path") == PYRIGHT_TARGET_REL
        and pyright_document_formatting_lsp.get("method")
        == PYRIGHT_DOCUMENT_FORMATTING_METHOD
        and pyright_document_formatting_lsp.get("capability")
        == PYRIGHT_DOCUMENT_FORMATTING_CAPABILITY
        and pyright_document_formatting_lsp.get("supported") is False
        and pyright_document_formatting_lsp.get("unsupported") is True
        and int(pyright_document_formatting_lsp.get("edit_count", -1)) == 0
        and pyright_execute_command.get("from_pyright_adapter") is True
        and pyright_execute_command.get("workspace_mounted") is True
        and pyright_execute_command.get("success") is False
        and pyright_execute_command_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_execute_command_lsp.get("method") == PYRIGHT_EXECUTE_COMMAND_METHOD
        and pyright_execute_command_lsp.get("capability")
        == "executeCommandProvider.commands"
        and pyright_execute_command_lsp.get("supported") is False
        and pyright_execute_command_lsp.get("unsupported") is True
        and pyright_execute_command_lsp.get("commands") == []
        and pyright_completion.get("from_pyright_adapter") is True
        and pyright_completion.get("workspace_mounted") is True
        and pyright_completion_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_completion_lsp.get("path") == PYRIGHT_COMPLETION_TARGET_REL
        and pyright_completion_lsp.get("position", {}).get("line") == PYRIGHT_COMPLETION_LINE
        and pyright_completion_lsp.get("position", {}).get("character")
        == PYRIGHT_COMPLETION_CHARACTER
        and PYRIGHT_SYMBOL in pyright_completion_lsp.get("matching_labels", [])
        and pyright_completion_resolve.get("from_pyright_adapter") is True
        and pyright_completion_resolve.get("workspace_mounted") is True
        and pyright_completion_resolve_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_completion_resolve_lsp.get("path") == PYRIGHT_COMPLETION_TARGET_REL
        and pyright_completion_resolve_lsp.get("position", {}).get("line")
        == PYRIGHT_COMPLETION_LINE
        and pyright_completion_resolve_lsp.get("position", {}).get("character")
        == PYRIGHT_COMPLETION_CHARACTER
        and pyright_completion_resolve_lsp.get("request_label") == PYRIGHT_SYMBOL
        and pyright_completion_resolve_lsp.get("resolved_label") == PYRIGHT_SYMBOL
        and pyright_diagnostics.get("from_pyright_adapter") is True
        and pyright_diagnostics.get("workspace_mounted") is True
        and pyright_diagnostics_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_diagnostics_lsp.get("path") == PYRIGHT_DIAGNOSTICS_TARGET_REL
        and pyright_diagnostics_lsp.get("position", {}).get("line")
        == PYRIGHT_DIAGNOSTICS_LINE
        and pyright_diagnostics_lsp.get("position", {}).get("character")
        == PYRIGHT_DIAGNOSTICS_CHARACTER
        and int(pyright_diagnostics_lsp.get("diagnostic_count", 0)) >= 1
        and any(
            isinstance(message, str) and PYRIGHT_DIAGNOSTICS_SYMBOL in message
            for message in pyright_diagnostic_messages
        )
        and pyright_code_actions.get("from_pyright_adapter") is True
        and pyright_code_actions.get("workspace_mounted") is True
        and pyright_code_actions_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_code_actions_lsp.get("path") == PYRIGHT_CODE_ACTION_TARGET_REL
        and pyright_code_actions_lsp.get("position", {}).get("line")
        == PYRIGHT_CODE_ACTION_LINE
        and pyright_code_actions_lsp.get("position", {}).get("character")
        == PYRIGHT_CODE_ACTION_CHARACTER
        and PYRIGHT_CODE_ACTION_KIND in pyright_code_actions_lsp.get("only", [])
        and isinstance(pyright_code_actions_lsp.get("actions"), list)
        and int(pyright_code_actions_lsp.get("action_count", -1)) >= 0
        and (
            int(pyright_code_actions_lsp.get("action_count", 0)) == 0
            or PYRIGHT_CODE_ACTION_KIND in pyright_code_action_kinds
        )
        and pyright_signature_help.get("from_pyright_adapter") is True
        and pyright_signature_help.get("workspace_mounted") is True
        and pyright_signature_help_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_signature_help_lsp.get("path") == PYRIGHT_SIGNATURE_TARGET_REL
        and pyright_signature_help_lsp.get("position", {}).get("line")
        == PYRIGHT_SIGNATURE_LINE
        and pyright_signature_help_lsp.get("position", {}).get("character")
        == PYRIGHT_SIGNATURE_CHARACTER
        and int(pyright_signature_help_lsp.get("signature_count", 0)) >= 1
        and pyright_signature_help_lsp.get("active_parameter") == 1
        and any(
            isinstance(label, str)
            and "left" in label
            and "right" in label
            for label in pyright_signature_labels
        )
        and pyright_hover.get("from_pyright_adapter") is True
        and pyright_hover.get("workspace_mounted") is True
        and pyright_hover_lsp.get("protocol") == "lsp-jsonrpc"
        and PYRIGHT_SYMBOL in pyright_hover_text
        and "int" in pyright_hover_text
        and pyright_type_definition.get("from_pyright_adapter") is True
        and pyright_type_definition.get("workspace_mounted") is True
        and pyright_type_definition_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_type_definition_lsp.get("path") == PYRIGHT_TYPE_TARGET_REL
        and pyright_type_definition_lsp.get("position", {}).get("line")
        == PYRIGHT_TYPE_LINE
        and pyright_type_definition_lsp.get("position", {}).get("character")
        == PYRIGHT_TYPE_CHARACTER
        and int(pyright_type_definition_lsp.get("type_definition_count", 0)) >= 1
        and any(
            isinstance(location, dict)
            and location.get("path") == PYRIGHT_TYPE_TARGET_REL
            and location.get("range", {}).get("start", {}).get("line") == 0
            for location in pyright_type_definition_locations
        )
        and pyright_declaration.get("from_pyright_adapter") is True
        and pyright_declaration.get("workspace_mounted") is True
        and pyright_declaration_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_declaration_lsp.get("path") == PYRIGHT_TARGET_REL
        and pyright_declaration_lsp.get("position", {}).get("line") == 3
        and pyright_declaration_lsp.get("position", {}).get("character") == 12
        and int(pyright_declaration_lsp.get("declaration_count", 0)) >= 1
        and any(
            isinstance(location, dict)
            and location.get("path") == PYRIGHT_TARGET_REL
            and location.get("range", {}).get("start", {}).get("line") == 0
            for location in pyright_declaration_locations
        )
        and pyright_call_hierarchy.get("from_pyright_adapter") is True
        and pyright_call_hierarchy.get("workspace_mounted") is True
        and pyright_call_hierarchy_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_call_hierarchy_lsp.get("path")
        == PYRIGHT_CALL_HIERARCHY_TARGET_REL
        and pyright_call_hierarchy_lsp.get("position", {}).get("line")
        == PYRIGHT_CALL_HIERARCHY_LINE
        and pyright_call_hierarchy_lsp.get("position", {}).get("character")
        == PYRIGHT_CALL_HIERARCHY_CHARACTER
        and int(pyright_call_hierarchy_lsp.get("item_count", 0)) >= 1
        and PYRIGHT_CALL_HIERARCHY_SYMBOL
        in pyright_call_hierarchy_lsp.get("item_names", [])
        and int(pyright_call_hierarchy_lsp.get("incoming_count", 0)) >= 1
        and PYRIGHT_CALL_HIERARCHY_CALLER
        in pyright_call_hierarchy_lsp.get("incoming_names", [])
        and pyright_call_hierarchy_outgoing.get("from_pyright_adapter") is True
        and pyright_call_hierarchy_outgoing.get("workspace_mounted") is True
        and pyright_call_hierarchy_outgoing_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_call_hierarchy_outgoing_lsp.get("path")
        == PYRIGHT_CALL_HIERARCHY_TARGET_REL
        and pyright_call_hierarchy_outgoing_lsp.get("position", {}).get("line")
        == PYRIGHT_CALL_HIERARCHY_OUTGOING_LINE
        and pyright_call_hierarchy_outgoing_lsp.get("position", {}).get("character")
        == PYRIGHT_CALL_HIERARCHY_OUTGOING_CHARACTER
        and int(pyright_call_hierarchy_outgoing_lsp.get("item_count", 0)) >= 1
        and PYRIGHT_CALL_HIERARCHY_CALLER
        in pyright_call_hierarchy_outgoing_lsp.get("item_names", [])
        and int(pyright_call_hierarchy_outgoing_lsp.get("outgoing_count", 0)) >= 1
        and PYRIGHT_CALL_HIERARCHY_SYMBOL
        in pyright_call_hierarchy_outgoing_lsp.get("outgoing_names", [])
        and pyright_document_highlight.get("from_pyright_adapter") is True
        and pyright_document_highlight.get("workspace_mounted") is True
        and pyright_document_highlight_lsp.get("protocol") == "lsp-jsonrpc"
        and int(pyright_document_highlight_lsp.get("highlight_count", 0)) >= 2
        and {0, 3}.issubset(pyright_highlight_start_lines)
        and pyright_prepare_rename.get("from_pyright_adapter") is True
        and pyright_prepare_rename.get("workspace_mounted") is True
        and pyright_prepare_rename_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_prepare_rename_range.get("start", {}).get("line") == 3
        and pyright_prepare_rename_range.get("start", {}).get("character") == 9
        and pyright_prepare_rename_range.get("end", {}).get("character") == 19
        and pyright_definition.get("from_pyright_adapter") is True
        and pyright_definition.get("workspace_mounted") is True
        and pyright_definition_lsp.get("protocol") == "lsp-jsonrpc"
        and int(pyright_definition_lsp.get("definition_count", 0)) >= 1
        and any(
            isinstance(location, dict)
            and location.get("path") == PYRIGHT_TARGET_REL
            and location.get("range", {}).get("start", {}).get("line") == 0
            for location in pyright_definition_locations
        )
        and pyright_references.get("from_pyright_adapter") is True
        and pyright_references.get("workspace_mounted") is True
        and pyright_references_lsp.get("protocol") == "lsp-jsonrpc"
        and int(pyright_references_lsp.get("reference_count", 0)) >= 2
        and {0, 3}.issubset(pyright_reference_start_lines)
        and lsp_apply_workspace_edit.get("from_lsp_workspace_edit") is True
        and lsp_apply_workspace_edit.get("from_self_managed") is True
        and lsp_apply_workspace_edit.get("workspace_mounted") is True
        and lsp_apply_workspace_edit_callback.get("success") is True
        and LSP_APPLY_EDIT_TARGET_REL
        in lsp_apply_workspace_edit.get("changed_paths", [])
        and lsp_apply_workspace_edit_readback.get("exists") is True
        and lsp_apply_workspace_edit_readback.get("content")
        == LSP_APPLY_EDIT_CONTENT_AFTER
        and lsp_apply_code_action.get("from_lsp_code_action") is True
        and lsp_apply_code_action.get("from_self_managed") is True
        and lsp_apply_code_action.get("workspace_mounted") is True
        and lsp_apply_code_action.get("action_title") == LSP_APPLY_CODE_ACTION_TITLE
        and lsp_apply_code_action.get("action_kind") == LSP_APPLY_CODE_ACTION_KIND
        and lsp_apply_code_action_callback.get("success") is True
        and LSP_APPLY_CODE_ACTION_TARGET_REL
        in lsp_apply_code_action.get("changed_paths", [])
        and lsp_apply_code_action_readback.get("exists") is True
        and lsp_apply_code_action_readback.get("content")
        == LSP_APPLY_CODE_ACTION_CONTENT_AFTER
        and lsp_format_document.get("from_lsp_formatting") is True
        and lsp_format_document.get("from_self_managed") is True
        and lsp_format_document.get("workspace_mounted") is True
        and lsp_format_document.get("method") == LSP_FORMAT_METHOD
        and int(lsp_format_document.get("edit_count", 0)) >= 1
        and lsp_format_document_callback.get("success") is True
        and LSP_FORMAT_TARGET_REL in lsp_format_document.get("changed_paths", [])
        and lsp_format_readback.get("exists") is True
        and lsp_format_readback.get("content") == LSP_FORMAT_CONTENT_AFTER
        and lsp_execute_command.get("from_lsp_execute_command") is True
        and lsp_execute_command.get("from_self_managed") is True
        and lsp_execute_command.get("workspace_mounted") is True
        and lsp_execute_command.get("method") == LSP_EXECUTE_COMMAND_METHOD
        and lsp_execute_command.get("command") == LSP_EXECUTE_COMMAND_NAME
        and lsp_execute_command.get("supported") is True
        and lsp_execute_command.get("unsupported") is False
        and lsp_execute_command_callback.get("success") is True
        and LSP_EXECUTE_COMMAND_TARGET_REL
        in lsp_execute_command.get("changed_paths", [])
        and lsp_execute_command_readback.get("exists") is True
        and lsp_execute_command_readback.get("content")
        == LSP_EXECUTE_COMMAND_CONTENT_AFTER
        and pyright_status.get("state") == "ready"
        and int(pyright_status.get("refresh_count", 0)) >= 1
        and pyright_rename.get("from_pyright_adapter") is True
        and pyright_rename.get("from_self_managed") is True
        and pyright_rename.get("workspace_mounted") is True
        and pyright_rename_callback.get("success") is True
        and PYRIGHT_TARGET_REL in pyright_rename.get("changed_paths", [])
        and pyright_rename_lsp.get("protocol") == "lsp-jsonrpc"
        and pyright_rename_lsp.get("new_name") == PYRIGHT_RENAMED_SYMBOL
        and pyright_rename_readback.get("exists") is True
        and pyright_rename_readback.get("content") == PYRIGHT_RENAMED_CONTENT
        and report.get("restart_ping", {}).get("from_ppc") is True
        and report.get("restart_ping", {}).get("from_restart_service") is True
        and report.get("restart_ping", {}).get("workspace_mounted") is True
        and report.get("restart_ping", {}).get("workspace_read", {}).get("content")
        == TARGET_CONTENT
        and restart_status.get("state") == "ready"
        and int(restart_status.get("restart_count", 0)) >= 1
        and int(restart_status.get("refresh_count", 0)) == 0
        and oneshot.get("success") is True
        and oneshot.get("plugin_overlay", {}).get("worker_exit_code") == 0
        and oneshot.get("plugin_result", {}).get("worker") == "oneshot_overlay"
        and oneshot_readback.get("exists") is True
        and oneshot_readback.get("content") == ONESHOT_CONTENT
        and report.get("crash_probe", {}).get("expected_failure") is True
        and "plugin.generic.crash_probe"
        not in report.get("status_after_crash", {}).get("connected_ppc_routes", [])
        and "plugin.generic.crash_recover_ping"
        not in report.get("status_after_crash", {}).get("connected_ppc_routes", [])
        and crash_status.get("state") == "stopped"
        and report.get("crash_recover_seed", {}).get("success") is True
        and report.get("crash_recover_ping", {}).get("from_crash_recovered_service")
        is True
        and report.get("crash_recover_ping", {}).get("from_ppc") is True
        and report.get("crash_recover_ping", {}).get("workspace_mounted") is True
        and report.get("crash_recover_ping", {}).get("echo") == "after-crash-recover"
        and report.get("crash_recover_ping", {}).get("workspace_read", {}).get(
            "content",
        )
        == CRASH_RECOVERY_CONTENT
        and "plugin.generic.crash_recover_ping"
        in report.get("status_after_crash_recover", {}).get("connected_ppc_routes", [])
        and crash_recover_status.get("state") == "ready"
        and int(crash_recover_status.get("restart_count", 0)) >= 1
        and report.get("hang_probe", {}).get("expected_failure") is True
        and "plugin.generic.hang_probe"
        not in report.get("status_after_hang", {}).get("connected_ppc_routes", [])
        and "plugin.generic.hang_recover_ping"
        not in report.get("status_after_hang", {}).get("connected_ppc_routes", [])
        and hang_status.get("state") == "stopped"
        and report.get("hang_recover_ping", {}).get("from_timeout_recovered_service")
        is True
        and report.get("hang_recover_ping", {}).get("from_ppc") is True
        and report.get("hang_recover_ping", {}).get("workspace_mounted") is True
        and report.get("hang_recover_ping", {}).get("echo")
        == "after-timeout-recover"
        and "plugin.generic.hang_recover_ping"
        in report.get("status_after_hang_recover", {}).get("connected_ppc_routes", [])
        and hang_recover_status.get("state") == "ready"
        and int(hang_recover_status.get("restart_count", 0)) >= 1
        and report.get("recover_probe_first", {}).get("expected_failure") is True
        and "plugin.generic.recover_probe"
        not in report.get("status_after_recover_failure", {}).get("connected_ppc_routes", [])
        and recover_failed_status.get("state") == "stopped"
        and report.get("recover_probe_second", {}).get("from_recovered_service") is True
        and report.get("recover_probe_second", {}).get("workspace_mounted") is True
        and "plugin.generic.recover_probe"
        in report.get("status_after_recover", {}).get("connected_ppc_routes", [])
        and recover_status.get("state") == "ready"
        and int(recover_status.get("restart_count", 0)) >= 1
        and isolated_plugin_gate.get("gate_pass") is True
        and final_metrics.get("orphan_layer_count") == 0
        and final_metrics.get("missing_layer_count") == 0
        and post_cleanup_metrics.get("active_leases") == 0
        and post_cleanup_metrics.get("orphan_layer_count") == 0
        and post_cleanup_metrics.get("missing_layer_count") == 0
        and int(processes_before_cleanup.get("count", 0)) >= 1
        and int(processes_after_cleanup.get("count", -1)) == 0
        and status_after_cleanup.get("connected_ppc_routes") == []
        and status_after_cleanup.get("connected_ppc_services") == []
        and status_after_cleanup.get("running_service_processes") == []
    )


def service_status(status_payload: dict[str, Any], service_id: str) -> dict[str, Any]:
    for plugin in status_payload.get("loaded_plugins", []):
        if not isinstance(plugin, dict):
            continue
        for service in plugin.get("services", []):
            if isinstance(service, dict) and service.get("key", {}).get("service_id") == service_id:
                return service
    return {}


def _status_count(status: dict[str, Any], key: str) -> int:
    try:
        return int(status.get(key, 0) or 0)
    except (TypeError, ValueError):
        return 0


def co_shared_refresh_summary(
    status_payload: dict[str, Any],
    first_service_id: str,
    second_service_id: str,
) -> dict[str, Any]:
    first = service_status(status_payload, first_service_id)
    second = service_status(status_payload, second_service_id)
    first_manifest = first.get("manifest_key")
    second_manifest = second.get("manifest_key")
    return {
        "first_service_id": first_service_id,
        "second_service_id": second_service_id,
        "first_state": first.get("state"),
        "second_state": second.get("state"),
        "first_manifest_key": first_manifest,
        "second_manifest_key": second_manifest,
        "same_manifest_key": bool(first_manifest)
        and first_manifest == second_manifest,
        "first_refresh_count": _status_count(first, "refresh_count"),
        "second_refresh_count": _status_count(second, "refresh_count"),
        "first_restart_count": _status_count(first, "restart_count"),
        "second_restart_count": _status_count(second, "restart_count"),
    }


def markdown_report(report: dict[str, Any]) -> str:
    refresh_status = service_status(report.get("status_after_refresh", {}), "harness")
    adapter_status = service_status(report.get("status_after_adapter", {}), "adapter_harness")
    pyright_status = service_status(report.get("status_after_pyright", {}), "pyright_harness")
    restart_status = service_status(report.get("status_after_restart", {}), "restart_harness")
    crash_status = service_status(report.get("status_after_crash", {}), "crash_harness")
    crash_recover_status = service_status(
        report.get("status_after_crash_recover", {}),
        "crash_harness",
    )
    hang_status = service_status(report.get("status_after_hang", {}), "hang_harness")
    hang_recover_status = service_status(
        report.get("status_after_hang_recover", {}),
        "hang_harness",
    )
    recover_failed_status = service_status(
        report.get("status_after_recover_failure", {}),
        "recover_harness",
    )
    recover_status = service_status(report.get("status_after_recover", {}), "recover_harness")
    runtime_bridge_status = service_status(
        report.get("status_after_runtime_bridge_ping", {}),
        "runtime_bridge",
    )
    health_fail_recover_status = service_status(
        report.get("status_after_health_fail_recover", {}),
        "health_fail_harness",
    )
    lines = [
        "# Rust Daemon Generic Plugin Benchmark",
        "",
        f"- run_id: `{report.get('run_id')}`",
        f"- sandbox_id: `{report.get('sandbox_id')}`",
        f"- gate_pass: `{report.get('gate_pass')}`",
        f"- daemon_spawn_ms: `{report.get('daemon_spawn_ms')}`",
        f"- connected_routes: `{report.get('status_after_ensure', {}).get('connected_ppc_routes')}`",
        f"- status_health_probe: `{report.get('status_after_health_probe', {}).get('service_health')}`",
        f"- connected_routes_after_health_probe: `{report.get('status_after_health_probe', {}).get('connected_ppc_routes')}`",
        f"- health_fail_recover_ping: `{report.get('health_fail_recover_ping')}`",
        f"- health_fail_recover_restart_count: `{health_fail_recover_status.get('restart_count')}`",
        f"- connected_routes_after_health_fail_recover: `{report.get('status_after_health_fail_recover', {}).get('connected_ppc_routes')}`",
        f"- ping_from_ppc: `{report.get('ping', {}).get('from_ppc')}`",
        f"- concurrent_ping: `{report.get('concurrent_ping')}`",
        f"- apply_from_self_managed: `{report.get('apply', {}).get('from_self_managed')}`",
        f"- readback_exists: `{report.get('readback', {}).get('exists')}`",
        f"- runtime_bridge_ping: `{report.get('runtime_bridge_ping')}`",
        f"- runtime_bridge_concurrent: `{report.get('runtime_bridge_concurrent')}`",
        f"- runtime_bridge_apply: `{report.get('runtime_bridge_apply')}`",
        f"- runtime_bridge_readback: `{report.get('runtime_bridge_readback', {}).get('content')}`",
        f"- runtime_bridge_concurrent_apply: `{report.get('runtime_bridge_concurrent_apply')}`",
        f"- runtime_bridge_concurrent_readback_a: `{report.get('runtime_bridge_concurrent_readback_a', {}).get('content')}`",
        f"- runtime_bridge_concurrent_readback_b: `{report.get('runtime_bridge_concurrent_readback_b', {}).get('content')}`",
        f"- runtime_bridge_refresh_count: `{runtime_bridge_status.get('refresh_count')}`",
        f"- lsp_bridge_query_symbols: `{report.get('lsp_bridge_query_symbols', {}).get('lsp')}`",
        f"- lsp_bridge_find_definitions: `{report.get('lsp_bridge_find_definitions', {}).get('lsp')}`",
        f"- lsp_bridge_find_references: `{report.get('lsp_bridge_find_references', {}).get('lsp')}`",
        f"- lsp_bridge_signature_help: `{report.get('lsp_bridge_signature_help', {}).get('lsp')}`",
        f"- lsp_bridge_document_highlight: `{report.get('lsp_bridge_document_highlight', {}).get('lsp')}`",
        f"- lsp_bridge_diagnostics: `{report.get('lsp_bridge_diagnostics', {}).get('lsp')}`",
        f"- lsp_bridge_code_actions: `{report.get('lsp_bridge_code_actions', {}).get('lsp')}`",
        f"- lsp_bridge_hover: `{report.get('lsp_bridge_hover', {}).get('lsp')}`",
        f"- lsp_bridge_rename: `{report.get('lsp_bridge_rename', {}).get('lsp')}`",
        f"- lsp_bridge_rename_readback: `{report.get('lsp_bridge_rename_readback', {}).get('content')}`",
        f"- lsp_bridge_apply_workspace_edit: `{report.get('lsp_bridge_apply_workspace_edit', {}).get('lsp')}`",
        f"- lsp_bridge_apply_readback: `{report.get('lsp_bridge_apply_readback', {}).get('content')}`",
        f"- lsp_bridge_apply_code_action: `{report.get('lsp_bridge_apply_code_action', {}).get('lsp')}`",
        f"- lsp_bridge_code_action_readback: `{report.get('lsp_bridge_code_action_readback', {}).get('content')}`",
        f"- lsp_bridge_format_document: `{report.get('lsp_bridge_format_document', {}).get('lsp')}`",
        f"- lsp_bridge_format_readback: `{report.get('lsp_bridge_format_readback', {}).get('content')}`",
        f"- lsp_bridge_execute_command: `{report.get('lsp_bridge_execute_command', {}).get('lsp')}`",
        f"- lsp_bridge_execute_command_readback: `{report.get('lsp_bridge_execute_command_readback', {}).get('content')}`",
        f"- apply_multi_callback_count: `{report.get('apply_multi', {}).get('callback_count')}`",
        f"- apply_multi_callbacks: `{report.get('apply_multi', {}).get('callbacks')}`",
        f"- multi_readback_a: `{report.get('multi_readback_a', {}).get('content')}`",
        f"- multi_readback_b: `{report.get('multi_readback_b', {}).get('content')}`",
        f"- shell_publish: `{report.get('shell_publish')}`",
        f"- shell_readback: `{report.get('shell_readback', {}).get('content')}`",
        f"- shell_refresh_ping: `{report.get('shell_refresh_ping')}`",
        f"- refresh_ping_from_ppc: `{report.get('refresh_ping', {}).get('from_ppc')}`",
        f"- refresh_workspace_read: `{report.get('refresh_ping', {}).get('workspace_read')}`",
        f"- refresh_count: `{refresh_status.get('refresh_count')}`",
        f"- adapter_package: `{report.get('adapter_query', {}).get('package')}`",
        f"- adapter_refresh_count: `{adapter_status.get('refresh_count')}`",
        f"- co_shared_refresh: `{report.get('co_shared_refresh')}`",
        f"- pyright_symbols: `{report.get('pyright_symbols', {}).get('lsp')}`",
        f"- pyright_workspace_symbols: `{report.get('pyright_workspace_symbols', {}).get('lsp')}`",
        f"- pyright_capabilities: `{report.get('pyright_capabilities', {}).get('lsp')}`",
        f"- pyright_document_formatting: `{report.get('pyright_document_formatting', {}).get('lsp')}`",
        f"- pyright_execute_command: `{report.get('pyright_execute_command', {}).get('lsp')}`",
        f"- pyright_completion: `{report.get('pyright_completion', {}).get('lsp')}`",
        f"- pyright_completion_resolve: `{report.get('pyright_completion_resolve', {}).get('lsp')}`",
        f"- pyright_diagnostics: `{report.get('pyright_diagnostics', {}).get('lsp')}`",
        f"- pyright_code_actions: `{report.get('pyright_code_actions', {}).get('lsp')}`",
        f"- pyright_signature_help: `{report.get('pyright_signature_help', {}).get('lsp')}`",
        f"- pyright_hover: `{report.get('pyright_hover', {}).get('lsp')}`",
        f"- pyright_type_definition: `{report.get('pyright_type_definition', {}).get('lsp')}`",
        f"- pyright_declaration: `{report.get('pyright_declaration', {}).get('lsp')}`",
        f"- pyright_call_hierarchy: `{report.get('pyright_call_hierarchy', {}).get('lsp')}`",
        f"- pyright_call_hierarchy_outgoing: `{report.get('pyright_call_hierarchy_outgoing', {}).get('lsp')}`",
        f"- pyright_document_highlight: `{report.get('pyright_document_highlight', {}).get('lsp')}`",
        f"- pyright_prepare_rename: `{report.get('pyright_prepare_rename', {}).get('lsp')}`",
        f"- pyright_definition: `{report.get('pyright_definition', {}).get('lsp')}`",
        f"- pyright_references: `{report.get('pyright_references', {}).get('lsp')}`",
        f"- lsp_apply_workspace_edit: `{report.get('lsp_apply_workspace_edit')}`",
        f"- lsp_apply_workspace_edit_readback: `{report.get('lsp_apply_workspace_edit_readback', {}).get('content')}`",
        f"- lsp_apply_code_action: `{report.get('lsp_apply_code_action')}`",
        f"- lsp_apply_code_action_readback: `{report.get('lsp_apply_code_action_readback', {}).get('content')}`",
        f"- lsp_format_document: `{report.get('lsp_format_document')}`",
        f"- lsp_format_readback: `{report.get('lsp_format_readback', {}).get('content')}`",
        f"- lsp_execute_command: `{report.get('lsp_execute_command')}`",
        f"- lsp_execute_command_readback: `{report.get('lsp_execute_command_readback', {}).get('content')}`",
        f"- pyright_refresh_count: `{pyright_status.get('refresh_count')}`",
        f"- pyright_rename: `{report.get('pyright_rename', {}).get('lsp')}`",
        f"- pyright_rename_callback: `{report.get('pyright_rename', {}).get('callback')}`",
        f"- pyright_rename_readback: `{report.get('pyright_rename_readback', {}).get('content')}`",
        f"- restart_ping_from_ppc: `{report.get('restart_ping', {}).get('from_ppc')}`",
        f"- restart_workspace_read: `{report.get('restart_ping', {}).get('workspace_read')}`",
        f"- restart_count: `{restart_status.get('restart_count')}`",
        f"- oneshot_success: `{report.get('oneshot', {}).get('success')}`",
        f"- oneshot_worker_exit_code: `{report.get('oneshot', {}).get('plugin_overlay', {}).get('worker_exit_code')}`",
        f"- oneshot_readback_exists: `{report.get('oneshot_readback', {}).get('exists')}`",
        f"- crash_probe: `{report.get('crash_probe')}`",
        f"- crash_service_state: `{crash_status.get('state')}`",
        f"- connected_routes_after_crash: `{report.get('status_after_crash', {}).get('connected_ppc_routes')}`",
        f"- crash_recover_seed: `{report.get('crash_recover_seed')}`",
        f"- crash_recover_ping: `{report.get('crash_recover_ping')}`",
        f"- crash_recover_restart_count: `{crash_recover_status.get('restart_count')}`",
        f"- connected_routes_after_crash_recover: `{report.get('status_after_crash_recover', {}).get('connected_ppc_routes')}`",
        f"- hang_probe: `{report.get('hang_probe')}`",
        f"- hang_service_state: `{hang_status.get('state')}`",
        f"- connected_routes_after_hang: `{report.get('status_after_hang', {}).get('connected_ppc_routes')}`",
        f"- hang_recover_ping: `{report.get('hang_recover_ping')}`",
        f"- hang_recover_restart_count: `{hang_recover_status.get('restart_count')}`",
        f"- connected_routes_after_hang_recover: `{report.get('status_after_hang_recover', {}).get('connected_ppc_routes')}`",
        f"- recover_probe_first: `{report.get('recover_probe_first')}`",
        f"- recover_service_state_after_failure: `{recover_failed_status.get('state')}`",
        f"- recover_probe_second: `{report.get('recover_probe_second')}`",
        f"- recover_service_restart_count: `{recover_status.get('restart_count')}`",
        f"- connected_routes_after_recover: `{report.get('status_after_recover', {}).get('connected_ppc_routes')}`",
        f"- isolated_plugin_gate: `{report.get('isolated_plugin_gate')}`",
        f"- retained_service_leases_before_cleanup: `{report.get('final_metrics', {}).get('active_leases')}`",
        f"- processes_before_cleanup: `{report.get('processes_before_cleanup')}`",
        f"- post_cleanup_active_leases: `{report.get('post_cleanup_metrics', {}).get('active_leases')}`",
        f"- processes_after_cleanup: `{report.get('processes_after_cleanup')}`",
        f"- connected_routes_after_cleanup: `{report.get('status_after_cleanup', {}).get('connected_ppc_routes')}`",
        f"- running_processes_after_cleanup: `{report.get('status_after_cleanup', {}).get('running_service_processes')}`",
        f"- final_orphans: `{report.get('final_metrics', {}).get('orphan_layer_count')}`",
        f"- final_missing: `{report.get('final_metrics', {}).get('missing_layer_count')}`",
        f"- post_cleanup_orphans: `{report.get('post_cleanup_metrics', {}).get('orphan_layer_count')}`",
        f"- post_cleanup_missing: `{report.get('post_cleanup_metrics', {}).get('missing_layer_count')}`",
        "",
    ]
    return "\n".join(lines)


if __name__ == "__main__":
    raise SystemExit(main())
