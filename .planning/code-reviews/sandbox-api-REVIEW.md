# Harsh Code Review — `backend/src/sandbox/api/`

**Scope:** `backend/src/sandbox/api/{__init__.py, facade.py, status.py, tool/*}`
**Focus:** naming, folder/file structure, import-dependency hygiene, extensibility, inheritance/interface use, future flexibility
**Verdict (TL;DR):** The public surface works, but the package is wearing two costumes at once — a "façade with a singleton" and "a bag of module-level functions." Both layers exist, both are exported, and neither is the contract. There is *zero* polymorphism on the public side despite a multi-provider, multi-transport architecture sitting one layer below. This is debt that *will* cost when you add a second transport, an alternate caller binding, or per-tenant audit.

Severity legend: **CRITICAL** (architecture trap), **HIGH** (will hurt soon), **MEDIUM** (will hurt eventually), **LOW** (taste/polish).

---

## 1. Folder & file structure

### 1.1 [HIGH] `api/` is a fake façade — it's a re-export layer with a hidden god-singleton

`api/__init__.py` constructs a process-global `_client = SandboxClient()` and then binds **17 module-level callables** to its bound methods:

```python
_client = SandboxClient()
create_sandbox = _client.create_sandbox
...
edit_file = _client.edit_file
```

This means consumers have **two equivalent entry points**:

- `from sandbox.api import edit_file` (module-level)
- `from sandbox.api import SandboxClient; SandboxClient().edit_file(...)` (class form)

And almost every external call site grabs the module-level form (confirmed via `grep -rn "from sandbox.api"` — `tools/sandbox/*.py`, `live_e2e/...`, `squad/runner.py`). So `SandboxClient` is *theatre*: nobody constructs it. Its constructor parameter `audit_sink` is never injected by callers, because the singleton is already bound. Result: a class that pretends to be DI-friendly but is wired to a global at import time.

**Pick one.** Either:
- Delete `SandboxClient` and keep the module-level functions (simpler, matches actual usage), or
- Delete the module-level singleton, force callers to take a `SandboxClient` (true DI, true testability, real injection point for `AuditSink` and provider).

You currently get the worst of both: a frozen audit_sink binding (always `None` at the singleton) plus the cost of carrying the class.

### 1.2 [MEDIUM] `tool/` subpackage exists but is *internal* — naming lies

`tool/__init__.py` says:

```python
"""Internal implementation modules for public sandbox tool verbs.
External callers should import from :mod:`sandbox.api`."""
```

…and re-exports nothing meaningful (`__all__ = ["edit", "raw_exec", "read", "shell", "write"]` are module names, not symbols). If it's internal, name it `_tool/` (PEP-8 leading underscore) or `_impl/`. Right now the public-looking name invites direct imports, and the typo would not be caught by a linter.

### 1.3 [MEDIUM] `_payload.py` is a leaky internal — naming inconsistent with siblings

`tool/_payload.py` uses the underscore prefix correctly. But everything else (`shell.py`, `read.py`, `write.py`, `edit.py`, `raw_exec.py`) is just bare names while being equally internal. Be consistent: either underscore the whole directory (move to `_impl/`) or stop pretending `_payload.py` is special. The current state is "I added underscores when I felt like it."

### 1.4 [LOW] `.DS_Store` checked into the tree

`backend/src/sandbox/api/.DS_Store` is present. Add to `.gitignore`. Pure noise.

### 1.5 [HIGH] No `protocol.py` / `base.py` for the public surface despite needing one

Every verb in `tool/` is a free function. There is **no `SandboxToolProtocol`, no `SandboxAPI` Protocol, no abstract base** that captures the public contract. Compare with what sits one layer below in the same package family:
- `sandbox/provider/protocol.py` (mentioned in `status.py` docstring) — provider gets a real Protocol.
- `sandbox/host/...` — host orchestration is segregated.

So providers have a Protocol, but the *public sandbox API itself* — the thing every external module depends on — has none. Test doubles, alt-impls, mock injection all become string-matching exercises (`monkeypatch.setattr("sandbox.api.edit_file", ...)`) instead of structural typing.

---

## 2. Naming conventions

### 2.1 [HIGH] Verb names collide between modules and symbols

- `tool/read.py` exports `read_file`
- `tool/write.py` exports `write_file`
- `tool/edit.py` exports `edit_file`
- `tool/shell.py` exports `shell`        ← inconsistent, should be `run_shell` or `shell_exec`
- `tool/raw_exec.py` exports `raw_exec`  ← module name == symbol name

Result: callers write `from sandbox.api.tool import shell as shell_module` / `from sandbox.api.tool import edit as edit_module` inside `facade.py` because the symbol shadows the module. That's a tell — the names are wrong. Use noun-based modules (`tool/file_read.py` exporting `read_file`, `tool/file_edit.py` exporting `edit_file`) or rename modules to match a single convention.

### 2.2 [MEDIUM] `api/status.py` is misnomered

`status.py` doesn't only do status — it does **create, start, stop, delete, set_labels, ensure_running, get_signed_preview_url, get_build_logs_url, list_sandboxes, list_snapshots, get_health**. That's lifecycle + discovery + control + diagnostics jammed into a file called `status.py`. The module docstring even admits it: *"Public sandbox status and **control** verbs."* Rename to `lifecycle.py` and split off `discovery.py` / `urls.py`, or — better — model these as proper Protocol methods on a `SandboxLifecycleAPI` so the split is by responsibility, not by happen-to-live-in-the-same-file.

### 2.3 [LOW] `caller_envelope` mixes English

`_payload.caller_envelope` returns a `dict[str, str]`. Fine. But the docstring says *"Forwards every audit-relevant field so the daemon can stitch runs by `run_id` / `agent_run_id` / `task_id`."* Then the function spreads `task_center_run_id`, `task_center_task_id`, `task_center_attempt_id`, `task_center_mission_id`, `task_center_request_id`, `tool_name`, `tool_id` conditionally. The "envelope" name suggests a fixed schema; in practice it's "all the audit fields, sometimes." Either it's a `CallerEnvelope` typed-dict / dataclass (and you assert presence), or rename to `caller_audit_fields()`.

### 2.4 [LOW] `_overlay_cwd` lives only in `shell.py`

`shell._overlay_cwd(cwd)` — returns `"."` when cwd is empty. This normalization belongs in `_payload.py` next to `caller_envelope`. Otherwise `write_file` / `edit_file` will silently develop divergent cwd semantics.

---

## 3. Import dependency chain

### 3.1 [CRITICAL] Function-local imports everywhere in `facade.py` mask circular issues

Every single method in `SandboxClient` does this:

```python
def stop_sandbox(self, sandbox_id: str) -> dict[str, Any]:
    from sandbox.api import status   # ← inside the method body
    return status.stop_sandbox(sandbox_id)
```

There are **15 instances of `from sandbox.api... import` *inside method bodies* in facade.py**. That is not a style choice; that is *the smell of circular imports nobody wants to fix*. Look at `api/__init__.py`:

```python
from sandbox.api.facade import SandboxClient
_client = SandboxClient()
```

→ `sandbox.api` imports `sandbox.api.facade`, which (if it eager-imported `sandbox.api.status`) would re-enter `sandbox.api`, triggering partial-module errors. The workaround is "import on every call." This:

1. **Pays the import cost on every call** (mitigated by `sys.modules` cache, but still measurable on hot paths like `edit_file`).
2. **Hides the dependency graph from tools** like `pydeps`, `import-linter`, `ruff TID`. You can never validate "facade only depends on tool submodules" because the imports are runtime-only.
3. **Defeats `from __future__ import annotations` benefits** for static analysis.
4. **Makes refactor-by-rename brittle** — IDE rename won't catch these.

**Fix:** invert the dependency. `facade.py` should be the canonical entry; `api/__init__.py` should be a thin re-export of `SandboxClient` and Pydantic models. Move the module-level singleton aliasing into a separate module (e.g., `sandbox.api.default`) so `facade` can eagerly import `tool/*` and `status` without cycling through `api/__init__.py`.

### 3.2 [HIGH] Double-layered re-export of `sandbox.models`

`sandbox/api/__init__.py` re-exports **12 model classes** from `sandbox.models`:

```python
from sandbox.models import (
    ConflictInfo, EditFileRequest, EditFileResult, GuardedResultBase,
    RawExecResult, ReadFileRequest, ReadFileResult, SandboxCaller,
    SandboxResultBase, SearchReplaceEdit, ShellRequest, ShellResult,
    WriteFileRequest, WriteFileResult,
)
```

But every internal module *also* imports them directly from `sandbox.models`. So you have two canonical import paths for `EditFileRequest`:

- `from sandbox.api import EditFileRequest` (external)
- `from sandbox.models import EditFileRequest` (internal)

Pick one. The current state means refactoring `sandbox.models` requires updating **two** import paths, and tests vs. production code drift on which path they use. Either:
- Make `sandbox.models` private (`sandbox._models`) and force `sandbox.api` as the only import path, **or**
- Drop the re-export and tell external callers to import from `sandbox.models` directly.

### 3.3 [MEDIUM] `api/status.py` reaches sideways into 4 sibling packages

```python
from sandbox.host.recovery import ensure_running as _ensure_running
from sandbox.host.setup import setup_after_create, setup_after_start
from sandbox.plugin import install as plugin_install
from sandbox.plugin import session as plugin_session
from sandbox.provider.registry import (
    dispose_adapter, get_adapter, get_default_provider, register_adapter,
)
```

…plus a runtime `from config import load_settings` buried in `_configured_sandbox_defaults`. So `api/status.py` directly couples to *five* other top-level concerns. That's not a "thin façade" — that's an **integration script masquerading as an API module**. The lifecycle orchestration (post-create hooks, plugin forgetting, adapter disposal) belongs in `sandbox.host.lifecycle` or `sandbox.lifecycle`, not the public API surface. The API should call **one** lifecycle method per verb.

Also: the runtime import of `config.load_settings` inside `_configured_sandbox_defaults` is the same "avoid circular import" smell as 3.1.

### 3.4 [LOW] Three modules each re-implement `_error_message`

`shell.py` and `edit.py` both define identical:

```python
def _error_message(error: BaseException) -> str:
    message = str(getattr(error, "message", "") or error)
    if message.startswith("internal_error: "):
        return message.removeprefix("internal_error: ")
    return message
```

Move to `_payload.py`. Three copies of the same helper is one too many.

---

## 4. Extensibility & future flexibility

### 4.1 [CRITICAL] Zero polymorphism on the public surface — adding a second backend will require touching every verb

Every verb is hardwired to **one** transport (`call_daemon_api` for `shell/read/write/edit`) and **one** provider call (`get_adapter(sandbox_id).exec` for `raw_exec`). There is no `Transport` Protocol, no `ToolBackend` interface. Imagine the natural extensions:

- A `LocalTransport` that runs against `subprocess` directly (for unit tests / CI without Daytona).
- A `MockTransport` for deterministic tests.
- A `RemoteAgentTransport` that buffers calls and replays.
- A `RecordingTransport` that captures requests/responses for golden tests.

Today every one of those requires monkey-patching `sandbox.host.daemon_client.call_daemon_api` — a module-level function imported into 5 places. **There is no seam.** The right shape is:

```python
class SandboxTransport(Protocol):
    async def call(self, sandbox_id: str, verb: str, payload: Mapping[str, Any], *, timeout: float) -> Mapping[str, Any]: ...
```

…and `SandboxClient.__init__(*, transport: SandboxTransport, audit_sink: AuditSink | None = None)`. Today the class accepts `audit_sink` and *only* `audit_sink`. That's a design that lasts exactly until you need to swap one of the other three external dependencies (`call_daemon_api`, `get_adapter`, `setup_after_*`).

### 4.2 [CRITICAL] Each verb hand-rolls its own try/except + publish boilerplate

`shell.py`, `raw_exec.py`, `read.py`, `write.py`, `edit.py` all open with:

```python
publish_operation_started(audit_sink, sandbox_id=..., operation=..., caller=..., payload=...)
try:
    ...
except Exception as exc:
    [optionally] conflict_result = _conflict_result_from_error(...)
    if conflict_result is not None:
        publish_operation_result(...)
        return conflict_result
    publish_operation_failed(audit_sink, ..., error=exc)
    raise
publish_operation_result(...)
```

This is **5 copies of the same control flow**. Worse, the copies have drifted:

- `shell.py` passes `payload={"cwd": _overlay_cwd(request.cwd)}` to `publish_operation_started`.
- `read.py` passes `payload={"path": request.path}`.
- `write.py` and `edit.py` pass `payload={"path": request.path}`.
- `raw_exec.py` passes `payload={"cwd": cwd or ""}` and `caller=None`.

Five copies, five subtly different shapes, no enforcement that they stay consistent. Refactor this into either:

1. A `@audited_verb("shell", payload_fn=lambda r: {"cwd": _overlay_cwd(r.cwd)})` decorator, **or**
2. An `AuditedVerb` base class with `pre_payload(request) -> dict`, `call(request) -> Result`, `conflict_result_from(exc) -> Result | None`. New verbs subclass instead of copy/paste.

Either way the boilerplate-to-logic ratio is currently ~3:1 per verb, and it grows linearly with the number of verbs.

### 4.3 [HIGH] No interface for `ConflictDetection` → each verb hardcodes its own substring matchers

`shell._is_shell_conflict`:

```python
"overlay capture refuses escaping symlink target" in lowered
or "unsupported tracked change kind: symlinkchange" in lowered
```

`edit._is_edit_conflict`:

```python
"anchor not found" in lowered
or "anchor occurrence count mismatch" in lowered
or "aborted_overlap" in lowered
or "old_text_not_found" in lowered
```

This is **string-matching on remote error messages** — among the most fragile patterns in the codebase. Change a wording in the daemon → silent regression: a conflict becomes an unhandled exception. There is no test that asserts these substrings exist in the daemon's actual error catalog. Replace with:

- A daemon-side error taxonomy (typed error codes in the response payload), or
- A `ConflictClassifier` Protocol with verb-specific implementations registered by name.

This is a **production-incident generator** waiting for the first daemon refactor.

### 4.4 [HIGH] Recovery logic in `edit.py` is bespoke — won't generalize to `write.py` / `shell.py`

`edit._call_edit_with_recovery` implements: "if transport blew up but the daemon may have committed, re-read the file and check if edits are visible." That's *exactly* the same race window as `write_file` — daemon writes the file, transport fails before response, client retries → double-write. But `write.py` has no recovery path. Either:

- `write_file` is broken (silent loss-of-write or duplicate-write under transport flakes), **or**
- `edit_file`'s recovery is over-engineered.

Pick one. The asymmetry is itself a bug. The pattern should live in a `TransientRecoveryPolicy` keyed by verb.

### 4.5 [MEDIUM] `_TRANSIENT_EDIT_ATTEMPTS = 2`, timeout constants are file-local magic numbers

```python
_EDIT_DAEMON_TIMEOUT_S = 20
_RECOVERY_READ_TIMEOUT_S = 20
_TRANSIENT_EDIT_ATTEMPTS = 2
```

…in `edit.py`. Meanwhile `shell.py` does:

```python
timeout=(60 if request.timeout is None else request.timeout) + 30,
```

…with `60` and `30` inline. And `read.py` / `write.py` both hardcode `timeout=60`. So timeouts are scattered across 4 files with 5 different conventions. Centralize in a `TimeoutPolicy` (or at minimum a constants module: `sandbox.api.timeouts`). When you add the 6th verb, you'll thank yourself.

### 4.6 [HIGH] `SandboxCaller` envelope can't evolve — every new field requires changes in `_payload.py`

`caller_envelope` enumerates fields manually. Adding a new audit dimension (e.g., `tenant_id`, `request_origin`, `trace_id`) means editing `_payload.py` *and* hoping every caller is updated. There's no schema versioning, no contract test against the daemon. Either auto-derive the envelope from `dataclasses.fields(caller)` (filtering empties) or codify it as a Pydantic model with `.model_dump(exclude_none=True)`. Manual enumeration of 11 fields is the third copy of the same bug pattern in this package (see also 4.2, 4.3).

### 4.7 [MEDIUM] No version negotiation between API client and daemon

`call_daemon_api(sandbox_id, "api.edit_file", payload, timeout=...)` — the verb name is a magic string, and there's no protocol version in the payload. The day you ship a backwards-incompatible payload schema (say, adding required `edit_mode: "search_replace" | "ast"`), you'll have to coordinate the rollout by hand. Two options:

- Version the verbs: `api.v2.edit_file`.
- Capability-negotiate at sandbox-create time, cache on the adapter.

Either way: pick one *before* the first incompatible change, not during the incident.

---

## 5. Inheritance / interface use

### 5.1 [CRITICAL] `SandboxClient` is not a class — it's a namespace dressed up as a class

It has:
- One field (`_audit_sink`).
- 17 methods, all of which immediately delegate to module-level functions.
- No subclasses (none in `grep`).
- No interface it implements (no `SandboxAPI` Protocol).
- No alternate constructors, no factory methods, no state machine.

This is **a module pretending to be a class**, which is the worst of both worlds. Either:

- **Promote it to a real class** with injected dependencies: `SandboxClient(transport, lifecycle, audit_sink)`. Then it earns its keep — alternate implementations (`TestSandboxClient`, `ReplaySandboxClient`) become first-class.
- **Demote it to a module** and stop the `from sandbox.api.facade import SandboxClient` ceremony.

Right now it's neither, and the cost is paid every time someone tries to test in isolation.

### 5.2 [HIGH] `GuardedResultBase` exists in `sandbox.models` but isn't leveraged in `api/`

`sandbox.models` has a real type hierarchy:

```
SandboxResultBase
├── RawExecResult
└── GuardedResultBase
    ├── WriteFileResult
    ├── EditFileResult
    └── ShellResult
ReadFileResult(SandboxResultBase)  # not guarded
```

But nowhere in `api/` does anything use this hierarchy. There is no generic `def _build_guarded_result(raw: dict, cls: type[T]) -> T` despite `_result_from_payload` in `shell.py`, `write.py`, `edit.py` doing the same job three times. A generic constructor over `GuardedResultBase` would collapse those three `_result_from_payload` functions to one. The base class is already there — use it.

### 5.3 [MEDIUM] No `Request` base class — duplicated `caller`/`description` plumbing

Every verb's request model has `caller: SandboxCaller` and most have `description: str | None`. There's no `SandboxRequestBase`. Audit wrapping (`caller_envelope(request.caller)` + `request.description or f"<verb> {request.path}"`) is repeated 4 times. A `RequestBase` with `.envelope()` and `.default_description(verb: str)` methods would make new verbs trivial.

### 5.4 [LOW] `ConflictInfo` constructed in two places with different field sets

- `shell._error_result`: `ConflictInfo(reason=reason, message=message)` — no `conflict_file`.
- `edit._conflict_result_from_error`: `ConflictInfo(reason=..., conflict_file=path, message=...)`.
- `_payload.conflict_from_payload`: `ConflictInfo(reason=..., conflict_file=...|None, message=...)`.

Three construction sites, three slightly different field patterns, no factory method on `ConflictInfo` itself. Add `ConflictInfo.rejected(reason, message)`, `ConflictInfo.overlap(path, message)` — make the *kind* of conflict a first-class constructor instead of "which fields happen to be set."

---

## 6. Test surface (implied by structure)

### 6.1 [HIGH] Nothing in `api/` is mockable without monkey-patching

Because every dependency is grabbed via a module-level function (`call_daemon_api`, `get_adapter`, `publish_operation_*`, `setup_after_*`), the only test strategy is `monkeypatch.setattr("sandbox.host.daemon_client.call_daemon_api", ...)`. That's brittle: every test re-creates the same fixture, and any code path that imports `call_daemon_api` under a different name (e.g., `from sandbox.host.daemon_client import call_daemon_api as _cda`) silently bypasses the patch.

**Direct consequence of 4.1 (no `Transport` Protocol)**. Fixing 4.1 fixes this.

### 6.2 [LOW] No contract test between `_payload.caller_envelope` output and the daemon's expected schema

You're sending strings into the daemon based on a hand-written field list. There's no schema enforcing that "`task_center_attempt_id` is the canonical name on both sides." A typo or rename one side or the other and audit attribution silently breaks.

---

## 7. Specific bugs surfaced during review

### 7.1 [MEDIUM] `_overlay_cwd("")` returns `"."` but `_overlay_cwd("   ")` also returns `"."`

```python
def _overlay_cwd(cwd: str | None) -> str:
    if cwd is None or not cwd.strip():
        return "."
    return cwd
```

`"   "` (whitespace) returns `"."` — fine. But `"  some/path  "` is passed through *with whitespace intact*, because the function tests `not cwd.strip()` but returns the raw `cwd`. Inconsistent: either strip, or don't. Today you might send the daemon `cwd="  some/path  "` and the daemon's `cd` will fail.

### 7.2 [MEDIUM] `_is_transient_transport_error` matches substrings — `"daytonaerror"` will match `"NotDaytonaError"`

```python
return any(
    marker in message
    for marker in (
        "daytonaerror", "failed to execute command", ...
    )
)
```

`"Failed to execute command"` is benign in many contexts — e.g., a user's `make` invocation returning non-zero with that phrase in stderr could get mis-classified as a *transport* error, triggering the recovery read. Add boundaries (`re.search(r"\bdaytonaerror\b", ...)`) or — better — get a typed error.

### 7.3 [LOW] `_edits_are_visible` returns `True` if every `new_text` appears anywhere in the content

```python
return bool(request.edits) and all(edit.new_text in content for edit in request.edits)
```

If `new_text="foo"` and `content` already contained `"foo"` before the edit (because the edit was meant to *change* `"bar"` → `"foo"` somewhere else), this returns `True` — falsely concluding the edit was applied. Combined with the transient-retry path in `_call_edit_with_recovery`, this can mask a real failure where the edit was *not* applied but `new_text` happened to already exist. Use sequence-of-edits delta or compare against a known pre-image hash.

### 7.4 [LOW] `int_from_payload` raises `TypeError` for `bool` but accepts string `"1"`

```python
if isinstance(value, bool):
    raise TypeError(...)
if isinstance(value, (str, int, float)):
    try: return int(value)
```

`True` → reject. `"true"` → ValueError → re-raised as TypeError. `1.5` → silently truncates to `1`. The string-and-float branch is too generous. If the daemon ever sends `"3.7"` it becomes `3`. Decide whether this is a strict-typed boundary or a coercion boundary, and document.

---

## 8. Recommended refactor (prioritized)

In dependency order — each unblocks the next.

1. **Introduce `SandboxTransport` Protocol** (`sandbox/api/transport.py`). Default impl wraps `call_daemon_api`. Inject into `SandboxClient`. *(Unblocks 4.1, 4.2, 6.1.)*

2. **Collapse the 5-copy try/audit/except boilerplate** into either a decorator or `AuditedVerb` base class. *(Unblocks 4.2.)*

3. **Move `_error_message`, `_overlay_cwd`, and conflict-substring matchers to `_payload.py`** (or `_internal/errors.py`). Single source of truth. *(Unblocks 3.4, 7.1, 7.2.)*

4. **Kill the `_client = SandboxClient()` aliasing in `__init__.py`.** Pick one entrypoint. *(Unblocks 1.1.)*

5. **Move `api/__init__.py`'s eager `from sandbox.api.facade import SandboxClient`** into a separate module so `facade.py` can eagerly import its dependencies. Delete all 15 method-local `from sandbox.api... import` statements. *(Unblocks 3.1.)*

6. **Rename `tool/` → `_impl/`** (or `_tool/`). Make the privacy contract structural, not documented. *(Unblocks 1.2, 1.3.)*

7. **Split `status.py` into `lifecycle.py`, `discovery.py`, `preview_urls.py`.** Move the post-create/post-start hooks into `sandbox.host.lifecycle`. *(Unblocks 2.2, 3.3.)*

8. **Add a daemon error taxonomy** (typed codes in payload). Retire substring matching. *(Unblocks 4.3, 7.2.)*

9. **Generalize `_call_edit_with_recovery` into a `TransientRecoveryPolicy`** applied uniformly to write/edit/shell. *(Unblocks 4.4.)*

10. **Version the daemon verbs** (`api.v1.edit_file`). *(Unblocks 4.7.)*

---

## 9. What's actually good

Credit where due:

- **Pydantic models live outside `api/`** (in `sandbox.models`) — clean separation between types and behavior. Don't break this.
- **`raw_exec` is correctly segregated** with a docstring warning it's not for agent use. The intent is there; it just isn't enforced.
- **`_payload.py` exists at all** — someone noticed the projection boilerplate was repetitive. Good instinct. Now finish the job.
- **`SandboxResultBase` / `GuardedResultBase` hierarchy in models** — the *types* are right. The API just doesn't leverage them.
- **`from __future__ import annotations` everywhere** — consistent and correct.
- **No untyped `**kwargs`** — every public function has a typed signature.

---

## 10. Summary

| Dimension              | Grade | One-line verdict                                                                |
| ---------------------- | :---: | ------------------------------------------------------------------------------- |
| Naming                 |  C    | `tool/` lies about visibility; `status.py` covers control; module/symbol clash. |
| Folder structure       |  C-   | Façade + namespace + bag-of-functions, all three layered on top of each other.  |
| Import hygiene         |  D    | 15 function-local imports in facade.py; double re-export; sideways coupling.    |
| Extensibility          |  D    | Zero polymorphism; substring-matched errors; verb-local timeouts.               |
| Inheritance/interfaces |  F    | `SandboxClient` is a fake class; `GuardedResultBase` unused; no Protocols.      |
| Future flexibility     |  D    | Can't swap transport, can't swap audit, can't version-negotiate, can't mock.    |

**Bottom line:** This package functions today because it's only got one transport (Daytona daemon), one provider, one audit sink, and one set of callers. The moment any of those plurals appears — a second transport for local testing, a second audit sink for per-tenant streams, a v2 daemon payload — every verb in `tool/` needs surgery. Spend the refactor budget now while there are only **5 verbs**, not after you've added the next 5.
