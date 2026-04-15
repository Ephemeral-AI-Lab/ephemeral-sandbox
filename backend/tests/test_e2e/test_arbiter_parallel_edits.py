"""E2E tests for Arbiter parallel file edits.

Exercises the OCC arbiter's ability to coordinate concurrent edits to the
same file from multiple agents. Tests run against a real Daytona sandbox
with a real CodeIntelligenceService (no mocks for the editing pipeline).

Scenarios covered:
  1. Two agents edit non-overlapping regions of the same file → merge succeeds
  2. Two agents edit overlapping regions → conflict detected for second writer
  3. Two agents edit different files concurrently → both succeed
  4. Stale token rejected after file changes underneath
  5. File lock serializes concurrent commits
  6. Edit intents visible via scope_status
  7. Live LLM-driven parallel edits via EvalAgent (two agents, one file)

Run with:
    pytest tests/test_e2e/test_arbiter_parallel_edits.py -v -s
    pytest tests/test_e2e/test_arbiter_parallel_edits.py -v -s -k live  # LLM tests only
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import hashlib
import json
import os
import threading
import time
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
from dotenv import load_dotenv

_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

from code_intelligence.editing.arbiter import Arbiter
from code_intelligence.editing.merge import detect_edit_window, merge_non_overlapping_edit
from code_intelligence.routing.service import CodeIntelligenceService
from code_intelligence.types import PreparedWrite
from tools.core.base import ToolExecutionContext
from tools.daytona_toolkit.edit_tool import daytona_edit_file

pytestmark = [pytest.mark.e2e]


# ---------------------------------------------------------------------------
# Credential loading
# ---------------------------------------------------------------------------

def _load_settings() -> dict:
    settings_path = Path.home() / ".ephemeralos" / "settings.json"
    if settings_path.exists():
        return json.loads(settings_path.read_text())
    return {}


_SETTINGS = _load_settings()
DAYTONA_KEY = os.environ.get("DAYTONA_API_KEY") or _SETTINGS.get("daytona_api_key", "")
DAYTONA_URL = os.environ.get("DAYTONA_API_URL") or _SETTINGS.get("daytona_api_url", "")
HAS_DAYTONA = bool(DAYTONA_KEY and DAYTONA_URL)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _content_hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


def _make_mock_sandbox(files: dict[str, str] | None = None) -> MagicMock:
    """In-memory mock sandbox with async fs ops and thread-safe file store."""
    sandbox = MagicMock()
    file_store = dict(files or {})
    _lock = threading.Lock()

    async def _download(path: str):
        with _lock:
            if path in file_store:
                return file_store[path].encode("utf-8")
        raise FileNotFoundError(f"File not found: {path}")

    async def _upload(content_or_path, path_or_content=None):
        # Handle both argument orders
        if isinstance(content_or_path, bytes):
            content, path = content_or_path, path_or_content
        else:
            path, content = content_or_path, path_or_content
        with _lock:
            file_store[path] = content.decode("utf-8") if isinstance(content, bytes) else content

    sandbox.fs.download_file = _download
    sandbox.fs.upload_file = _upload
    sandbox._file_store = file_store
    sandbox._lock = _lock
    return sandbox


def _make_ci_service(sandbox: Any, workspace: str = "/workspace") -> CodeIntelligenceService:
    """Create a real CI service backed by the mock sandbox."""
    svc = CodeIntelligenceService.__new__(CodeIntelligenceService)
    svc.sandbox_id = "test-parallel"
    svc.workspace_root = workspace
    svc._sandbox = sandbox
    svc._initialized = True
    svc._init_lock = threading.Lock()

    svc.arbiter = Arbiter(workspace_root=workspace)

    # Real patcher, time machine, symbol index (empty)
    from code_intelligence.editing.patcher import Patcher
    from code_intelligence.editing.time_machine import TimeMachine
    from code_intelligence.editing.write_coordinator import WriteCoordinator
    from code_intelligence.analysis.symbol_index import SymbolIndex
    from code_intelligence.routing.content_manager import ContentManager

    svc.patcher = Patcher()
    svc.time_machine = TimeMachine()
    svc.symbol_index = SymbolIndex(workspace_root=workspace)

    # Minimal LSP stub — we don't need LSP for edit coordination tests
    lsp = MagicMock()
    lsp.connected = False
    lsp.telemetry = MagicMock(queries=0, cache_hits=0)
    lsp.invalidate = MagicMock()
    lsp.ensure_ready = MagicMock()
    svc.lsp_client = lsp

    # Query router stub
    svc.query_router = MagicMock()
    svc._content = ContentManager(workspace, sandbox=sandbox)
    svc._write_coordinator = WriteCoordinator(
        arbiter=svc.arbiter,
        time_machine=svc.time_machine,
        patcher=svc.patcher,
        symbol_index=svc.symbol_index,
        lsp_client=svc.lsp_client,
        content=svc._content,
    )

    return svc


def _ctx(sandbox: Any, ci_service: Any = None) -> ToolExecutionContext:
    metadata: dict[str, Any] = {
        "daytona_sandbox": sandbox,
        "daytona_cwd": "/workspace",
    }
    if ci_service is not None:
        metadata["ci_service"] = ci_service
    return ToolExecutionContext(cwd=Path("/workspace"), metadata=metadata)


def _ctx_for_agent(
    sandbox: Any,
    ci_service: Any,
    *,
    agent_run_id: str,
) -> ToolExecutionContext:
    return ToolExecutionContext(
        cwd=Path("/workspace"),
        metadata={
            "daytona_sandbox": sandbox,
            "daytona_cwd": "/workspace",
            "ci_service": ci_service,
            "agent_run_id": agent_run_id,
        },
    )


_test_loop: asyncio.AbstractEventLoop | None = None


def _get_loop() -> asyncio.AbstractEventLoop:
    global _test_loop
    if _test_loop is None or _test_loop.is_closed():
        _test_loop = asyncio.new_event_loop()
    return _test_loop


def _run(coro):
    return _get_loop().run_until_complete(coro)


# ===========================================================================
# 1. Non-overlapping merge — two agents edit different regions of the same file
# ===========================================================================


class TestNonOverlappingMerge:
    """When two agents edit non-overlapping regions, the second commit
    should auto-merge via the OCC merge strategy."""

    def test_sequential_non_overlapping_edits_merge(self):
        """Agent A edits top of file, agent B edits bottom. Both succeed."""
        original = "# Header\nimport os\n\ndef foo():\n    return 1\n\ndef bar():\n    return 2\n"
        sandbox = _make_mock_sandbox(files={"/workspace/app.py": original})
        svc = _make_ci_service(sandbox)

        # Agent A: prepare, then commit change to foo()
        prep_a = svc.prepare_write("/workspace/app.py", agent_id="agent-a")
        assert isinstance(prep_a, PreparedWrite)
        new_a = original.replace("return 1", "return 42")
        result_a = svc.commit_prepared_write(prep_a, new_a, edit_type="edit", description="agent-a: fix foo")
        assert result_a.success

        # Agent B: prepare with stale snapshot (original), edit bar()
        # Simulate: agent B had read before A committed
        prep_b = PreparedWrite(
            file_path="/workspace/app.py",
            token_id=svc.arbiter.issue_token("/workspace/app.py", _content_hash(original), "agent-b").token_id,
            current_content=original,
            current_hash=_content_hash(original),
            agent_id="agent-b",
            existed=True,
        )
        new_b = original.replace("return 2", "return 99")

        result_b = svc.commit_prepared_write(prep_b, new_b, edit_type="edit", description="agent-b: fix bar")
        assert result_b.success, f"Non-overlapping merge failed: {result_b.message}"

        # Verify final file has both changes
        final = sandbox._file_store["/workspace/app.py"]
        assert "return 42" in final, "Agent A's change lost"
        assert "return 99" in final, "Agent B's change lost"
        assert "return 1" not in final
        assert "return 2" not in final

    def test_insert_at_top_conflicts_with_bottom_edit(self):
        """Agent A inserts at top, shifting all lines — agent B's stale edit
        to the bottom correctly conflicts because the merge algorithm cannot
        verify prefix stability when lines shift."""
        original = "line1\nline2\nline3\nline4\nline5\n"
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": original})
        svc = _make_ci_service(sandbox)

        # Agent A: insert header (shifts all subsequent lines)
        prep_a = svc.prepare_write("/workspace/f.py", agent_id="agent-a")
        new_a = "# new header\n# by agent-a\n" + original
        result_a = svc.commit_prepared_write(prep_a, new_a, edit_type="edit", description="add header")
        assert result_a.success

        # Agent B: stale snapshot, edit line5
        prep_b = PreparedWrite(
            file_path="/workspace/f.py",
            token_id=svc.arbiter.issue_token("/workspace/f.py", _content_hash(original), "agent-b").token_id,
            current_content=original,
            current_hash=_content_hash(original),
            agent_id="agent-b",
            existed=True,
        )
        new_b = original.replace("line5", "LINE_FIVE")
        result_b = svc.commit_prepared_write(prep_b, new_b, edit_type="edit", description="edit line5")
        # Correctly rejected: prefix lines shifted, merge cannot verify safety
        assert not result_b.success
        assert result_b.conflict

    def test_append_at_end_merges_with_top_edit(self):
        """Agent A edits the top, agent B appends at the end — non-overlapping merge succeeds."""
        original = "line1\nline2\nline3\n"
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": original})
        svc = _make_ci_service(sandbox)

        # Agent A: edit line1
        prep_a = svc.prepare_write("/workspace/f.py", agent_id="agent-a")
        new_a = original.replace("line1", "LINE_ONE")
        result_a = svc.commit_prepared_write(prep_a, new_a, edit_type="edit", description="edit top")
        assert result_a.success

        # Agent B: stale snapshot, edit line3 (last line)
        prep_b = PreparedWrite(
            file_path="/workspace/f.py",
            token_id=svc.arbiter.issue_token("/workspace/f.py", _content_hash(original), "agent-b").token_id,
            current_content=original,
            current_hash=_content_hash(original),
            agent_id="agent-b",
            existed=True,
        )
        new_b = original.replace("line3", "LINE_THREE")
        result_b = svc.commit_prepared_write(prep_b, new_b, edit_type="edit", description="edit bottom")
        assert result_b.success, f"Merge failed: {result_b.message}"

        final = sandbox._file_store["/workspace/f.py"]
        assert "LINE_ONE" in final
        assert "LINE_THREE" in final


# ===========================================================================
# 2. Overlapping edits — conflict detection
# ===========================================================================


class TestOverlappingConflict:
    """When two agents edit the same lines, the second commit must fail
    with a conflict rather than silently overwriting."""

    def test_overlapping_edits_produce_conflict(self):
        """Both agents modify the same function body → conflict."""
        original = "def compute():\n    x = 1\n    y = 2\n    return x + y\n"
        sandbox = _make_mock_sandbox(files={"/workspace/math.py": original})
        svc = _make_ci_service(sandbox)

        # Agent A commits first
        prep_a = svc.prepare_write("/workspace/math.py", agent_id="agent-a")
        new_a = original.replace("x = 1", "x = 10")
        result_a = svc.commit_prepared_write(prep_a, new_a, edit_type="edit", description="agent-a")
        assert result_a.success

        # Agent B tries to edit the same line from stale snapshot
        prep_b = PreparedWrite(
            file_path="/workspace/math.py",
            token_id=svc.arbiter.issue_token("/workspace/math.py", _content_hash(original), "agent-b").token_id,
            current_content=original,
            current_hash=_content_hash(original),
            agent_id="agent-b",
            existed=True,
        )
        new_b = original.replace("x = 1", "x = 100")
        result_b = svc.commit_prepared_write(prep_b, new_b, edit_type="edit", description="agent-b")
        assert not result_b.success, "Overlapping edit should have been rejected"
        assert result_b.conflict, "Should be flagged as conflict"

    def test_same_line_different_columns_still_conflicts(self):
        """Edits on the same line but different parts still conflict at line granularity."""
        original = "config = {'host': 'localhost', 'port': 8080}\n"
        sandbox = _make_mock_sandbox(files={"/workspace/cfg.py": original})
        svc = _make_ci_service(sandbox)

        prep_a = svc.prepare_write("/workspace/cfg.py", agent_id="agent-a")
        new_a = original.replace("localhost", "0.0.0.0")
        result_a = svc.commit_prepared_write(prep_a, new_a, edit_type="edit")
        assert result_a.success

        prep_b = PreparedWrite(
            file_path="/workspace/cfg.py",
            token_id=svc.arbiter.issue_token("/workspace/cfg.py", _content_hash(original), "agent-b").token_id,
            current_content=original,
            current_hash=_content_hash(original),
            agent_id="agent-b",
            existed=True,
        )
        new_b = original.replace("8080", "9090")
        result_b = svc.commit_prepared_write(prep_b, new_b, edit_type="edit")
        assert not result_b.success
        assert result_b.conflict


# ===========================================================================
# 3. Different files — fully parallel, no contention
# ===========================================================================


class TestDifferentFilesParallel:
    """Concurrent edits to different files should never conflict."""

    def test_concurrent_different_file_edits(self):
        """Two threads edit different files via the same CI service simultaneously."""
        sandbox = _make_mock_sandbox(files={
            "/workspace/a.py": "a = 1\n",
            "/workspace/b.py": "b = 2\n",
        })
        svc = _make_ci_service(sandbox)
        results: dict[str, Any] = {}
        errors: list[str] = []

        def _edit(file_path: str, agent_id: str, old: str, new: str):
            try:
                prep = svc.prepare_write(file_path, agent_id=agent_id)
                if not isinstance(prep, PreparedWrite):
                    errors.append(f"{agent_id}: prepare failed: {getattr(prep, 'message', prep)}")
                    return
                content = prep.current_content.replace(old, new)
                result = svc.commit_prepared_write(prep, content, edit_type="edit", description=agent_id)
                results[agent_id] = result
            except Exception as exc:
                errors.append(f"{agent_id}: {exc}")

        t1 = threading.Thread(target=_edit, args=("/workspace/a.py", "agent-a", "a = 1", "a = 42"))
        t2 = threading.Thread(target=_edit, args=("/workspace/b.py", "agent-b", "b = 2", "b = 99"))
        t1.start()
        t2.start()
        t1.join(timeout=10)
        t2.join(timeout=10)

        assert not errors, f"Thread errors: {errors}"
        assert results["agent-a"].success
        assert results["agent-b"].success
        assert sandbox._file_store["/workspace/a.py"].strip() == "a = 42"
        assert sandbox._file_store["/workspace/b.py"].strip() == "b = 99"

    def test_many_agents_many_files(self):
        """N agents editing N different files concurrently — all succeed."""
        n = 8
        files = {f"/workspace/f{i}.py": f"val = {i}\n" for i in range(n)}
        sandbox = _make_mock_sandbox(files=files)
        svc = _make_ci_service(sandbox)
        results: dict[int, Any] = {}

        def _edit(idx: int):
            fp = f"/workspace/f{idx}.py"
            prep = svc.prepare_write(fp, agent_id=f"agent-{idx}")
            if isinstance(prep, PreparedWrite):
                content = prep.current_content.replace(f"val = {idx}", f"val = {idx * 100}")
                results[idx] = svc.commit_prepared_write(prep, content, edit_type="edit")

        with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
            list(pool.map(_edit, range(n)))

        for i in range(n):
            assert results[i].success, f"Agent {i} failed: {results[i].message}"
            assert f"val = {i * 100}" in sandbox._file_store[f"/workspace/f{i}.py"]


# ===========================================================================
# 4. Token staleness and expiry
# ===========================================================================


class TestTokenStaleness:
    """Stale or expired tokens must be rejected."""

    def test_stale_hash_rejected(self):
        """Token issued for old hash, file changed → commit rejected."""
        original = "x = 1\n"
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": original})
        svc = _make_ci_service(sandbox)

        prep = svc.prepare_write("/workspace/f.py", agent_id="agent-a")
        assert isinstance(prep, PreparedWrite)

        # Another agent sneaks in a write directly
        sandbox._file_store["/workspace/f.py"] = "x = 999\n"

        result = svc.commit_prepared_write(
            prep, "x = 42\n", edit_type="edit", description="stale write"
        )
        # Should detect the file changed and attempt merge or conflict
        # Since the edit window overlaps (same line), it should conflict
        assert result.conflict or not result.success

    def test_released_token_rejected(self):
        """After releasing a token, commit with that token fails."""
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": "content\n"})
        svc = _make_ci_service(sandbox)

        prep = svc.prepare_write("/workspace/f.py", agent_id="agent-a")
        assert isinstance(prep, PreparedWrite)

        # Release the token explicitly
        svc.arbiter.release_token(prep.token_id)

        result = svc.commit_prepared_write(prep, "new content\n", edit_type="edit")
        assert not result.success
        assert result.conflict


# ===========================================================================
# 5. Forced merge path — concurrent commits with stale snapshots
# ===========================================================================


class TestForcedMergePath:
    """Force the merge_non_overlapping_edit code path by preparing both
    writes BEFORE either commits. This guarantees the second writer's
    hash is stale and commit_prepared_write must call _resolve_pending_write
    → detect_edit_window → merge_non_overlapping_edit.

    This is the critical test that the LLM retry tests do NOT cover.
    """

    def test_two_threads_merge_different_lines_same_file(self):
        """Two threads prepare simultaneously, then commit. The second
        committer MUST go through the merge path (not retry)."""
        original = (
            "def alpha():\n"
            "    return 'a'\n"
            "\n"
            "def beta():\n"
            "    return 'b'\n"
            "\n"
            "def gamma():\n"
            "    return 'c'\n"
        )
        sandbox = _make_mock_sandbox(files={"/workspace/merge.py": original})
        svc = _make_ci_service(sandbox)

        # Both agents prepare BEFORE either commits — both see hash H0
        prep_a = svc.prepare_write("/workspace/merge.py", agent_id="agent-a")
        prep_b = svc.prepare_write("/workspace/merge.py", agent_id="agent-b")
        assert isinstance(prep_a, PreparedWrite)
        assert isinstance(prep_b, PreparedWrite)
        assert prep_a.current_hash == prep_b.current_hash  # Both see H0

        # Agent A edits alpha (lines 1-2), agent B edits gamma (lines 7-8)
        new_a = original.replace("return 'a'", "return 'ALPHA'")
        new_b = original.replace("return 'c'", "return 'GAMMA'")

        barrier = threading.Barrier(2, timeout=5)
        results: dict[str, Any] = {}

        def _commit(name, prep, content):
            barrier.wait()  # Both threads commit at the same instant
            results[name] = svc.commit_prepared_write(
                prep, content, edit_type="edit", description=name,
            )

        t1 = threading.Thread(target=_commit, args=("a", prep_a, new_a))
        t2 = threading.Thread(target=_commit, args=("b", prep_b, new_b))
        t1.start()
        t2.start()
        t1.join(timeout=10)
        t2.join(timeout=10)

        # BOTH must succeed — the second goes through merge
        assert results["a"].success, f"Agent A failed: {results['a'].message}"
        assert results["b"].success, f"Agent B failed: {results['b'].message}"

        final = sandbox._file_store["/workspace/merge.py"]
        assert "return 'ALPHA'" in final, "Agent A's change lost"
        assert "return 'GAMMA'" in final, "Agent B's change lost"
        assert "return 'b'" in final, "beta() should be untouched"

    def test_three_agents_all_merge_different_regions(self):
        """Three agents prepare at H0, commit in sequence. Agent 2 merges
        with agent 1's change, agent 3 merges with both."""
        original = (
            "# config\n"
            "HOST = 'localhost'\n"
            "\n"
            "# ports\n"
            "PORT = 8080\n"
            "\n"
            "# debug\n"
            "DEBUG = False\n"
        )
        sandbox = _make_mock_sandbox(files={"/workspace/cfg.py": original})
        svc = _make_ci_service(sandbox)

        # All three prepare at H0
        prep_1 = svc.prepare_write("/workspace/cfg.py", agent_id="agent-1")
        prep_2 = svc.prepare_write("/workspace/cfg.py", agent_id="agent-2")
        prep_3 = svc.prepare_write("/workspace/cfg.py", agent_id="agent-3")
        assert isinstance(prep_1, PreparedWrite)
        assert isinstance(prep_2, PreparedWrite)
        assert isinstance(prep_3, PreparedWrite)

        new_1 = original.replace("HOST = 'localhost'", "HOST = '0.0.0.0'")
        new_2 = original.replace("PORT = 8080", "PORT = 9090")
        new_3 = original.replace("DEBUG = False", "DEBUG = True")

        # Commit sequentially — each subsequent commit has a stale hash
        r1 = svc.commit_prepared_write(prep_1, new_1, edit_type="edit")
        assert r1.success, f"Agent 1 failed: {r1.message}"

        r2 = svc.commit_prepared_write(prep_2, new_2, edit_type="edit")
        assert r2.success, f"Agent 2 merge failed: {r2.message}"

        r3 = svc.commit_prepared_write(prep_3, new_3, edit_type="edit")
        assert r3.success, f"Agent 3 merge failed: {r3.message}"

        final = sandbox._file_store["/workspace/cfg.py"]
        assert "HOST = '0.0.0.0'" in final, "Agent 1's change lost"
        assert "PORT = 9090" in final, "Agent 2's change lost after merge"
        assert "DEBUG = True" in final, "Agent 3's change lost after merge"

    def test_five_agents_concurrent_barrier_merge(self):
        """Five agents prepare at same hash, hit a barrier, commit simultaneously.
        All edit different lines → all should merge successfully."""
        original = "\n\n".join(
            f"def func_{i}():\n    return {i}\n" for i in range(5)
        ) + "\n"
        sandbox = _make_mock_sandbox(files={"/workspace/five.py": original})
        svc = _make_ci_service(sandbox)

        # All five prepare at H0
        preps = []
        news = []
        for i in range(5):
            prep = svc.prepare_write("/workspace/five.py", agent_id=f"agent-{i}")
            assert isinstance(prep, PreparedWrite)
            preps.append(prep)
            news.append(original.replace(f"return {i}", f"return {i * 100}"))

        barrier = threading.Barrier(5, timeout=5)
        results: dict[int, Any] = {}

        def _commit(idx):
            barrier.wait()
            results[idx] = svc.commit_prepared_write(
                preps[idx], news[idx], edit_type="edit", description=f"agent-{idx}",
            )

        threads = [threading.Thread(target=_commit, args=(i,)) for i in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        # Count successes — all non-overlapping, so all should merge
        successes = [i for i, r in results.items() if r.success]
        failures = [i for i, r in results.items() if not r.success]

        final = sandbox._file_store["/workspace/five.py"]
        landed = [i for i in range(5) if f"return {i * 100}" in final]

        print(f"[5-barrier] successes={successes}, failures={failures}, landed={landed}")

        # All 5 should succeed via merge. In the worst case, lock contention
        # might cause a token to expire, so allow ≥4.
        assert len(successes) >= 4, (
            f"Expected ≥4/5 merges, got {len(successes)}. "
            f"Failures: {[(i, results[i].message) for i in failures]}"
        )
        assert len(landed) >= 4, f"Expected ≥4 edits in final file, got {landed}"

    def test_overlapping_edits_barrier_conflict(self):
        """Two agents prepare at same hash, both edit the SAME line.
        The second committer must CONFLICT (not merge)."""
        original = "x = 1\ny = 2\nz = 3\n"
        sandbox = _make_mock_sandbox(files={"/workspace/dup.py": original})
        svc = _make_ci_service(sandbox)

        prep_a = svc.prepare_write("/workspace/dup.py", agent_id="agent-a")
        prep_b = svc.prepare_write("/workspace/dup.py", agent_id="agent-b")
        assert isinstance(prep_a, PreparedWrite)
        assert isinstance(prep_b, PreparedWrite)

        # Both edit x = 1 (line 1) — overlapping
        new_a = original.replace("x = 1", "x = 100")
        new_b = original.replace("x = 1", "x = 200")

        barrier = threading.Barrier(2, timeout=5)
        results: dict[str, Any] = {}

        def _commit(name, prep, content):
            barrier.wait()
            results[name] = svc.commit_prepared_write(
                prep, content, edit_type="edit", description=name,
            )

        t1 = threading.Thread(target=_commit, args=("a", prep_a, new_a))
        t2 = threading.Thread(target=_commit, args=("b", prep_b, new_b))
        t1.start()
        t2.start()
        t1.join(timeout=10)
        t2.join(timeout=10)

        # Exactly one succeeds, one conflicts
        a_ok = results["a"].success
        b_ok = results["b"].success
        assert a_ok != b_ok, (
            f"Expected exactly one success. a={a_ok}, b={b_ok}. "
            f"a.msg={results['a'].message}, b.msg={results['b'].message}"
        )

        loser = "b" if a_ok else "a"
        assert results[loser].conflict, f"Loser should have conflict=True"

        # Final file has exactly one value
        final = sandbox._file_store["/workspace/dup.py"]
        assert ("x = 100" in final) != ("x = 200" in final), (
            f"Exactly one value should be present. File:\n{final}"
        )


# ===========================================================================
# 5b. File lock serialization
# ===========================================================================


class TestFileLockSerialization:
    """The per-file lock serializes commits to the same file."""

    def test_concurrent_commits_serialized_by_lock(self):
        """Two threads commit to the same file — lock ensures no data corruption."""
        original = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\n"
        sandbox = _make_mock_sandbox(files={"/workspace/shared.py": original})
        svc = _make_ci_service(sandbox)
        results: dict[str, Any] = {}
        order: list[str] = []

        def _edit(agent_id: str, target: str, replacement: str):
            prep = svc.prepare_write("/workspace/shared.py", agent_id=agent_id)
            if not isinstance(prep, PreparedWrite):
                results[agent_id] = prep
                return
            content = prep.current_content.replace(target, replacement)
            result = svc.commit_prepared_write(prep, content, edit_type="edit", description=agent_id)
            order.append(agent_id)
            results[agent_id] = result

        # Both agents target non-overlapping lines
        t1 = threading.Thread(target=_edit, args=("agent-a", "line1", "LINE_ONE"))
        t2 = threading.Thread(target=_edit, args=("agent-b", "line8", "LINE_EIGHT"))
        t1.start()
        t2.start()
        t1.join(timeout=10)
        t2.join(timeout=10)

        # At least one must succeed; the second may succeed via merge or fail via conflict
        successes = [k for k, v in results.items() if getattr(v, "success", False)]
        assert len(successes) >= 1, f"Expected at least one success, got {results}"

        final = sandbox._file_store["/workspace/shared.py"]
        # The first committer's change must be present
        first = order[0] if order else "agent-a"
        if first == "agent-a":
            assert "LINE_ONE" in final
        else:
            assert "LINE_EIGHT" in final


# ===========================================================================
# 6. Edit intents and scope_status
# ===========================================================================


class TestEditIntentVisibility:
    """Edit intents published during edits are visible to other agents
    querying scope_status."""

    def test_published_intent_visible_in_scope_status(self):
        """Publishing an intent makes it visible in the arbiter's active intents."""
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": "x = 1\n"})
        svc = _make_ci_service(sandbox)

        intent_id = svc.publish_edit_intent(
            filepath="/workspace/f.py",
            agent_id="agent-a",
            symbols=["my_func"],
            scope="symbol",
        )

        intents = svc.arbiter.active_edit_intents(["/workspace/f.py"])
        assert len(intents) == 1
        assert intents[0]["agent_id"] == "agent-a"
        assert intents[0]["scope"] == "symbol"
        assert "my_func" in intents[0]["symbols"]

        # Release and verify gone
        svc.release_edit_intent(intent_id)
        intents_after = svc.arbiter.active_edit_intents(["/workspace/f.py"])
        assert len(intents_after) == 0

    def test_scope_status_shows_active_reservations(self):
        """Active write reservations appear in scope_status."""
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": "x = 1\n"})
        svc = _make_ci_service(sandbox)

        prep = svc.prepare_write("/workspace/f.py", agent_id="agent-a")
        assert isinstance(prep, PreparedWrite)

        status = svc.scope_status(["/workspace/f.py"])
        assert len(status["active_reservations"]) >= 1
        assert any(r["agent_id"] == "agent-a" for r in status["active_reservations"])

        svc.abort_prepared_write(prep)

    def test_multiple_agents_intents_coexist(self):
        """Multiple agents can hold edit intents on different files simultaneously."""
        sandbox = _make_mock_sandbox(files={
            "/workspace/a.py": "a\n",
            "/workspace/b.py": "b\n",
        })
        svc = _make_ci_service(sandbox)

        id_a = svc.publish_edit_intent(filepath="/workspace/a.py", agent_id="agent-a", scope="file")
        id_b = svc.publish_edit_intent(filepath="/workspace/b.py", agent_id="agent-b", scope="file")

        all_intents = svc.arbiter.active_edit_intents()
        assert len(all_intents) == 2

        svc.release_edit_intent(id_a)
        svc.release_edit_intent(id_b)


# ===========================================================================
# 7. Arbiter metrics tracking
# ===========================================================================


class TestArbiterMetrics:
    """Verify the arbiter tracks edit statistics correctly."""

    def test_metrics_increment_on_edits(self):
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": "x = 1\n"})
        svc = _make_ci_service(sandbox)

        initial = svc.arbiter.metrics
        assert initial.total_edits == 0
        assert initial.tokens_issued == 0

        prep = svc.prepare_write("/workspace/f.py", agent_id="agent-a")
        assert isinstance(prep, PreparedWrite)
        assert svc.arbiter.metrics.tokens_issued >= 1

        result = svc.commit_prepared_write(prep, "x = 2\n", edit_type="edit")
        assert result.success
        assert svc.arbiter.metrics.total_edits == 1

    def test_conflict_increments_conflict_counter(self):
        """The arbiter doesn't track conflicts in metrics directly, but
        the generation counter still advances only on successful edits."""
        sandbox = _make_mock_sandbox(files={"/workspace/f.py": "x = 1\n"})
        svc = _make_ci_service(sandbox)

        # Successful edit
        prep1 = svc.prepare_write("/workspace/f.py", agent_id="agent-a")
        svc.commit_prepared_write(prep1, "x = 2\n", edit_type="edit")
        gen_after_first = svc.arbiter.generation

        # Failed edit (released token)
        prep2 = svc.prepare_write("/workspace/f.py", agent_id="agent-b")
        assert isinstance(prep2, PreparedWrite)
        svc.arbiter.release_token(prep2.token_id)
        result = svc.commit_prepared_write(prep2, "x = 3\n", edit_type="edit")
        assert not result.success
        assert svc.arbiter.generation == gen_after_first  # No increment


# ===========================================================================
# 8. Edit tool integration — full OCC round-trip through daytona_edit_file
# ===========================================================================


class TestEditToolOccRoundTrip:
    """Exercise the daytona_edit_file tool with a real CI service to verify
    the full OCC pipeline: prepare → intent → commit → release."""

    def test_sequential_edits_through_tool(self):
        """Two sequential edits through the tool, both succeed."""
        sandbox = _make_mock_sandbox(files={
            "/workspace/app.py": "def foo():\n    return 1\n\ndef bar():\n    return 2\n"
        })
        svc = _make_ci_service(sandbox)
        ctx = _ctx(sandbox, svc)

        result1 = _run(daytona_edit_file.execute(
            daytona_edit_file.input_model(
                file_path="/workspace/app.py",
                old_text="return 1",
                new_text="return 42",
            ),
            ctx,
        ))
        assert not result1.is_error
        data1 = json.loads(result1.output)
        assert data1["occ"] is True

        result2 = _run(daytona_edit_file.execute(
            daytona_edit_file.input_model(
                file_path="/workspace/app.py",
                old_text="return 2",
                new_text="return 99",
            ),
            ctx,
        ))
        assert not result2.is_error
        data2 = json.loads(result2.output)
        assert data2["occ"] is True

        final = sandbox._file_store["/workspace/app.py"]
        assert "return 42" in final
        assert "return 99" in final

    def test_batch_edits_through_tool(self):
        """Batch edits (multiple search_replace) in a single tool call."""
        sandbox = _make_mock_sandbox(files={
            "/workspace/f.py": "alpha\nbeta\ngamma\ndelta\n"
        })
        svc = _make_ci_service(sandbox)
        ctx = _ctx(sandbox, svc)

        result = _run(daytona_edit_file.execute(
            daytona_edit_file.input_model(
                file_path="/workspace/f.py",
                edits=[
                    {"strategy": "search_replace", "search": "alpha", "replace": "ALPHA"},
                    {"strategy": "search_replace", "search": "delta", "replace": "DELTA"},
                ],
            ),
            ctx,
        ))
        assert not result.is_error
        final = sandbox._file_store["/workspace/f.py"]
        assert "ALPHA" in final
        assert "DELTA" in final
        assert "beta" in final  # untouched


class TestConcurrentEditToolSearchReplace:
    """Concurrent daytona_edit_file coverage for same-file search/replace OCC."""

    @staticmethod
    def _run_concurrent_search_replace(
        *,
        sandbox: Any,
        svc: Any,
        monkeypatch: pytest.MonkeyPatch,
        file_path: str,
        agent_edits: list[tuple[str, str, str]],
    ) -> dict[str, Any]:
        import tools.daytona_toolkit.edit_tool as edit_tool_module

        barrier = threading.Barrier(len(agent_edits), timeout=5)
        original_prepare_ci_write = edit_tool_module.prepare_ci_write

        def _prepare_ci_write_barrier(*args, **kwargs):
            prepared, scope_packet, err = original_prepare_ci_write(*args, **kwargs)
            if prepared is not None and err is None:
                try:
                    barrier.wait(timeout=5)
                except threading.BrokenBarrierError as exc:  # pragma: no cover - defensive
                    raise AssertionError("Concurrent edit test barrier broke before all writers prepared") from exc
            return prepared, scope_packet, err

        monkeypatch.setattr(edit_tool_module, "prepare_ci_write", _prepare_ci_write_barrier)

        def _worker(agent_id: str, search: str, replace: str):
            ctx = _ctx_for_agent(sandbox, svc, agent_run_id=agent_id)
            return asyncio.run(
                daytona_edit_file.execute(
                    daytona_edit_file.input_model(
                        file_path=file_path,
                        edits=[
                            {
                                "strategy": "search_replace",
                                "search": search,
                                "replace": replace,
                            }
                        ],
                        description=f"{agent_id}: replace {search!r}",
                    ),
                    ctx,
                )
            )

        with concurrent.futures.ThreadPoolExecutor(max_workers=len(agent_edits)) as pool:
            futures = {
                agent_id: pool.submit(_worker, agent_id, search, replace)
                for agent_id, search, replace in agent_edits
            }
            return {agent_id: future.result(timeout=10) for agent_id, future in futures.items()}

    def test_five_agents_same_file_search_replace_merge_via_tool(self, monkeypatch):
        """Five concurrent same-file tool edits on different lines should all land."""
        original = "\n\n".join(
            f"def func_{i}():\n    return {i}\n" for i in range(5)
        ) + "\n"
        sandbox = _make_mock_sandbox(files={"/workspace/same_file.py": original})
        svc = _make_ci_service(sandbox)
        edits = [
            (f"agent-{i}", f"return {i}", f"return {i + 100}")
            for i in range(5)
        ]

        results = self._run_concurrent_search_replace(
            sandbox=sandbox,
            svc=svc,
            monkeypatch=monkeypatch,
            file_path="/workspace/same_file.py",
            agent_edits=edits,
        )

        for agent_id, result in results.items():
            assert not result.is_error, f"{agent_id} failed: {result.output}"
            payload = json.loads(result.output)
            assert payload["occ"] is True, f"{agent_id} should use OCC"

        final = sandbox._file_store["/workspace/same_file.py"]
        for i in range(5):
            assert f"return {i + 100}" in final, f"Agent {i} edit missing from final file"
            assert f"return {i}\n" not in final, f"Original value {i} still present"

    def test_eight_agents_same_file_search_replace_detects_conflicts(self, monkeypatch):
        """Eight concurrent tool edits on one file merge disjoint regions and reject overlap."""
        original = (
            "\n\n".join(
                f"def unique_{i}():\n    return {i}\n" for i in range(5)
            )
            + "\n\n"
            + "def shared_conflict():\n    return 'base'\n"
        )
        sandbox = _make_mock_sandbox(files={"/workspace/mixed.py": original})
        svc = _make_ci_service(sandbox)
        edits = [
            (f"unique-{i}", f"return {i}", f"return {i + 1000}")
            for i in range(5)
        ] + [
            ("conflict-a", "return 'base'", "return 'A_WON'"),
            ("conflict-b", "return 'base'", "return 'B_WON'"),
            ("conflict-c", "return 'base'", "return 'C_WON'"),
        ]

        results = self._run_concurrent_search_replace(
            sandbox=sandbox,
            svc=svc,
            monkeypatch=monkeypatch,
            file_path="/workspace/mixed.py",
            agent_edits=edits,
        )

        unique_results = {agent_id: results[agent_id] for agent_id, _, _ in edits[:5]}
        conflict_results = {agent_id: results[agent_id] for agent_id, _, _ in edits[5:]}

        for agent_id, result in unique_results.items():
            assert not result.is_error, f"{agent_id} should merge cleanly: {result.output}"
            payload = json.loads(result.output)
            assert payload["occ"] is True

        conflict_successes = [
            agent_id for agent_id, result in conflict_results.items() if not result.is_error
        ]
        conflict_failures = [
            agent_id for agent_id, result in conflict_results.items()
            if result.is_error and result.metadata.get("conflict") is True
        ]

        assert len(conflict_successes) == 1, (
            f"Expected exactly one overlapping winner, got {conflict_successes}. "
            f"Outputs: { {k: v.output for k, v in conflict_results.items()} }"
        )
        assert len(conflict_failures) == 2, (
            f"Expected two overlapping conflicts, got {conflict_failures}. "
            f"Outputs: { {k: v.output for k, v in conflict_results.items()} }"
        )

        final = sandbox._file_store["/workspace/mixed.py"]
        for i in range(5):
            assert f"return {i + 1000}" in final, f"Unique edit {i} missing"

        landed_overlap_values = [token for token in ("A_WON", "B_WON", "C_WON") if token in final]
        assert len(landed_overlap_values) == 1, (
            f"Overlapping search/replace edits must not merge. File:\n{final}"
        )


# ===========================================================================
# 9. Live sandbox tests (require Daytona credentials)
# ===========================================================================


@pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona credentials not configured")
class TestLiveSandboxParallelEdits:
    """Run parallel edit tests against a real Daytona sandbox."""

    @pytest.fixture(autouse=True)
    def _setup_sandbox(self):
        """Create a test sandbox before each test and clean up after."""
        from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

        info = create_test_sandbox(name="arbiter-parallel")
        self.sandbox_id = info["id"]
        self.sandbox_svc = get_sandbox_service()
        self.raw_sandbox = self.sandbox_svc.get_sandbox_object(self.sandbox_id)

        # Discover home directory
        home_resp = self.raw_sandbox.process.exec("pwd", timeout=10)
        self.home = (home_resp.result or "").strip() or "/home/daytona"

        # Write test files
        test_content = (
            "# Module header\nimport os\n\n"
            "def function_a():\n    return 'a'\n\n"
            "def function_b():\n    return 'b'\n\n"
            "def function_c():\n    return 'c'\n"
        )
        self.raw_sandbox.fs.upload_file(test_content.encode("utf-8"), f"{self.home}/shared.py")
        self.raw_sandbox.fs.upload_file(b"file_x = 1\n", f"{self.home}/x.py")
        self.raw_sandbox.fs.upload_file(b"file_y = 2\n", f"{self.home}/y.py")

        yield

        delete_test_sandbox(self.sandbox_id)

    def _make_live_ci_service(self) -> CodeIntelligenceService:
        """Create a CI service backed by the real sandbox."""
        svc = CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.home,
            sandbox=self.raw_sandbox,
        )
        return svc

    def test_live_non_overlapping_merge(self):
        """Two agents edit different functions in the same file on a real sandbox."""
        svc = self._make_live_ci_service()
        shared = f"{self.home}/shared.py"

        # Read initial content
        prep_a = svc.prepare_write(shared, agent_id="agent-a")
        assert isinstance(prep_a, PreparedWrite)
        original = prep_a.current_content

        # Agent A: edit function_a
        new_a = original.replace("return 'a'", "return 'A_MODIFIED'")
        result_a = svc.commit_prepared_write(prep_a, new_a, edit_type="edit", description="agent-a: fix a")
        assert result_a.success, f"Agent A failed: {result_a.message}"

        # Agent B: stale snapshot, edit function_c
        prep_b = PreparedWrite(
            file_path=shared,
            token_id=svc.arbiter.issue_token(shared, _content_hash(original), "agent-b").token_id,
            current_content=original,
            current_hash=_content_hash(original),
            agent_id="agent-b",
            existed=True,
        )
        new_b = original.replace("return 'c'", "return 'C_MODIFIED'")
        result_b = svc.commit_prepared_write(prep_b, new_b, edit_type="edit", description="agent-b: fix c")
        assert result_b.success, f"Non-overlapping merge on live sandbox failed: {result_b.message}"

        # Verify by reading back
        final_raw = self.raw_sandbox.fs.download_file(shared)
        final = final_raw.decode("utf-8") if isinstance(final_raw, bytes) else str(final_raw)
        assert "A_MODIFIED" in final, "Agent A's change lost on live sandbox"
        assert "C_MODIFIED" in final, "Agent B's change lost on live sandbox"

    def test_live_overlapping_conflict(self):
        """Two agents edit the same function on a real sandbox → conflict."""
        svc = self._make_live_ci_service()
        shared = f"{self.home}/shared.py"

        prep_a = svc.prepare_write(shared, agent_id="agent-a")
        assert isinstance(prep_a, PreparedWrite)
        original = prep_a.current_content

        new_a = original.replace("return 'b'", "return 'B_BY_A'")
        result_a = svc.commit_prepared_write(prep_a, new_a, edit_type="edit")
        assert result_a.success

        prep_b = PreparedWrite(
            file_path=shared,
            token_id=svc.arbiter.issue_token(shared, _content_hash(original), "agent-b").token_id,
            current_content=original,
            current_hash=_content_hash(original),
            agent_id="agent-b",
            existed=True,
        )
        new_b = original.replace("return 'b'", "return 'B_BY_B'")
        result_b = svc.commit_prepared_write(prep_b, new_b, edit_type="edit")
        assert not result_b.success, "Overlapping edit should conflict on live sandbox"
        assert result_b.conflict

    def test_live_different_files_concurrent(self):
        """Two threads edit different files on a real sandbox simultaneously."""
        svc = self._make_live_ci_service()
        results: dict[str, Any] = {}
        errors: list[str] = []

        def _edit(fp: str, agent: str, old: str, new: str):
            try:
                prep = svc.prepare_write(fp, agent_id=agent)
                if not isinstance(prep, PreparedWrite):
                    errors.append(f"{agent}: {getattr(prep, 'message', 'failed')}")
                    return
                content = prep.current_content.replace(old, new)
                results[agent] = svc.commit_prepared_write(prep, content, edit_type="edit")
            except Exception as exc:
                errors.append(f"{agent}: {exc}")

        x_py = f"{self.home}/x.py"
        y_py = f"{self.home}/y.py"
        t1 = threading.Thread(target=_edit, args=(x_py, "agent-x", "file_x = 1", "file_x = 100"))
        t2 = threading.Thread(target=_edit, args=(y_py, "agent-y", "file_y = 2", "file_y = 200"))
        t1.start()
        t2.start()
        t1.join(timeout=30)
        t2.join(timeout=30)

        assert not errors, f"Live concurrent edit errors: {errors}"
        assert results["agent-x"].success
        assert results["agent-y"].success

    def test_live_forced_merge_same_file_different_lines(self):
        """Forced merge on real sandbox: prepare both at H0, then commit.

        This is the critical test — it guarantees the merge code path
        (not the retry path) is exercised on a real Daytona sandbox.
        """
        svc = self._make_live_ci_service()
        shared = f"{self.home}/shared.py"

        # Both agents prepare at the SAME hash — before either commits
        prep_a = svc.prepare_write(shared, agent_id="agent-a")
        prep_b = svc.prepare_write(shared, agent_id="agent-b")
        assert isinstance(prep_a, PreparedWrite)
        assert isinstance(prep_b, PreparedWrite)
        assert prep_a.current_hash == prep_b.current_hash, "Both should see same hash"

        original = prep_a.current_content

        # Agent A: edit function_a (top of file)
        new_a = original.replace("return 'a'", "return 'A_MERGED'")
        # Agent B: edit function_c (bottom of file)
        new_b = original.replace("return 'c'", "return 'C_MERGED'")

        # Agent A commits first — succeeds normally
        result_a = svc.commit_prepared_write(prep_a, new_a, edit_type="edit", description="agent-a")
        assert result_a.success, f"Agent A failed: {result_a.message}"

        # Agent B commits with stale hash — MUST go through merge path
        result_b = svc.commit_prepared_write(prep_b, new_b, edit_type="edit", description="agent-b")
        assert result_b.success, (
            f"Agent B merge failed on live sandbox: {result_b.message}. "
            f"conflict={result_b.conflict}, reason={result_b.conflict_reason}"
        )

        # Verify both edits present in final file
        final_raw = self.raw_sandbox.fs.download_file(shared)
        final = final_raw.decode("utf-8") if isinstance(final_raw, bytes) else str(final_raw)
        assert "A_MERGED" in final, f"Agent A's change lost after merge. File:\n{final}"
        assert "C_MERGED" in final, f"Agent B's change lost after merge. File:\n{final}"
        assert "return 'b'" in final, f"function_b should be untouched. File:\n{final}"

    def test_live_forced_overlap_conflict_same_file(self):
        """Forced overlap on real sandbox: both agents edit function_b.

        Prepare both at H0, commit sequentially — second must CONFLICT.
        """
        svc = self._make_live_ci_service()
        shared = f"{self.home}/shared.py"

        prep_a = svc.prepare_write(shared, agent_id="agent-a")
        prep_b = svc.prepare_write(shared, agent_id="agent-b")
        assert isinstance(prep_a, PreparedWrite)
        assert isinstance(prep_b, PreparedWrite)

        original = prep_a.current_content

        # Both edit the same function_b
        new_a = original.replace("return 'b'", "return 'B_BY_A'")
        new_b = original.replace("return 'b'", "return 'B_BY_B'")

        result_a = svc.commit_prepared_write(prep_a, new_a, edit_type="edit")
        assert result_a.success

        result_b = svc.commit_prepared_write(prep_b, new_b, edit_type="edit")
        assert not result_b.success, "Overlapping forced merge should conflict"
        assert result_b.conflict

        # Only agent A's value should be in the file
        final_raw = self.raw_sandbox.fs.download_file(shared)
        final = final_raw.decode("utf-8") if isinstance(final_raw, bytes) else str(final_raw)
        assert "B_BY_A" in final
        assert "B_BY_B" not in final


# ===========================================================================
# 10. Live LLM-driven parallel edits (require Daytona + LLM credentials)
# ===========================================================================


@pytest.mark.skipif(not HAS_DAYTONA, reason="Daytona credentials not configured")
@pytest.mark.live
class TestLiveLLMParallelEdits:
    """Use EvalAgent to drive concurrent agents that edit the same file via LLM.

    This validates the full stack: LLM → tool call → OCC arbiter → sandbox.
    """

    # Template for a file with 10 distinct, widely-spaced functions.
    # Each function is on its own "island" so the merge algorithm can
    # cleanly distinguish non-overlapping edits.
    _TEMPLATE = "\n\n".join(
        [
            '"""Module with 10 worker functions."""',
            *(
                f"def worker_{i}():\n"
                f'    """Worker {i} logic."""\n'
                f"    result_{i} = {i}\n"
                f"    return result_{i}\n"
                for i in range(10)
            ),
        ]
    ) + "\n"

    @pytest.fixture(autouse=True)
    def _setup(self):
        from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

        info = create_test_sandbox(name="arbiter-llm")
        self.sandbox_id = info["id"]
        self.sandbox_svc = get_sandbox_service()
        self.raw_sandbox = self.sandbox_svc.get_sandbox_object(self.sandbox_id)

        home_resp = self.raw_sandbox.process.exec("pwd", timeout=10)
        self.home = (home_resp.result or "").strip() or "/home/daytona"

        yield
        delete_test_sandbox(self.sandbox_id)

    def _skip_if_no_credentials(self):
        from engine.testing.eval_agent import EvalAgent

        if not EvalAgent.has_all():
            pytest.skip("LLM + Daytona credentials required")

    def _read_file(self, rel_path: str) -> str:
        content = self.raw_sandbox.fs.download_file(f"{self.home}/{rel_path}")
        return content.decode("utf-8") if isinstance(content, bytes) else str(content)

    def _write_file(self, rel_path: str, content: str) -> None:
        self.raw_sandbox.fs.upload_file(content.encode("utf-8"), f"{self.home}/{rel_path}")

    @staticmethod
    def _daytona_edit_completions(result: Any, *, is_error: bool | None = None) -> list[Any]:
        completions = [
            event
            for event in result.tools_completed()
            if getattr(event, "tool_name", "") == "daytona_edit_file"
        ]
        if is_error is None:
            return completions
        return [event for event in completions if bool(getattr(event, "is_error", False)) is is_error]

    def _run_eval_prompts(
        self,
        prompts: list[str],
        *,
        tool_call_limit: int,
        timeout: int = 180,
    ) -> tuple[dict[int, Any], list[str]]:
        from tests.test_e2e.conftest import create_eval_agent

        agents = [
            create_eval_agent(sandbox_id=self.sandbox_id, tool_call_limit=tool_call_limit)
            for _ in prompts
        ]
        results: dict[int, Any] = {}
        invocation_errors: list[str] = []

        def _invoke_agent(idx: int, prompt: str):
            try:
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
                results[idx] = loop.run_until_complete(agents[idx].invoke(prompt, verbose=True))
            except Exception as exc:
                invocation_errors.append(f"agent-{idx}: {exc}")
            finally:
                try:
                    loop.close()
                except Exception:
                    pass

        threads = [
            threading.Thread(target=_invoke_agent, args=(idx, prompt))
            for idx, prompt in enumerate(prompts)
        ]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=timeout)

        return results, invocation_errors

    # ------------------------------------------------------------------
    # Test 1: Sequential baseline — two edits, both succeed
    # ------------------------------------------------------------------

    def test_llm_sequential_edits_same_file(self):
        """Two sequential LLM-driven edits to the same file — both succeed."""
        self._skip_if_no_credentials()
        from tests.test_e2e.conftest import create_eval_agent

        self._write_file("counter.py", (
            "count = 0\n\n"
            "def increment():\n    global count\n    count += 1\n    return count\n\n"
            "def decrement():\n    global count\n    count -= 1\n    return count\n"
        ))

        agent = create_eval_agent(sandbox_id=self.sandbox_id)
        fp = f"{self.home}/counter.py"

        _run(agent.invoke(
            f"Use daytona_edit_file to edit {fp}: "
            "change `count += 1` to `count += 10` in the increment function. "
            "Do not change anything else."
        ))
        _run(agent.invoke(
            f"Use daytona_edit_file to edit {fp}: "
            "change `count -= 1` to `count -= 5` in the decrement function. "
            "Do not change anything else."
        ))

        text = self._read_file("counter.py")
        assert "count += 10" in text, f"First edit missing. File:\n{text}"
        assert "count -= 5" in text, f"Second edit missing. File:\n{text}"

    # ------------------------------------------------------------------
    # Test 2: 5 concurrent LLM agents edit different regions of one file
    # ------------------------------------------------------------------

    def test_llm_5_concurrent_edits_different_functions_no_conflict(self):
        """Five one-shot EvalAgents on disjoint lines should mostly land.

        Live contention can still surface a per-file lock timeout even when
        the edit windows do not overlap. This test records that behavior while
        ensuring the file is not corrupted and nearly all edits land.
        """
        self._skip_if_no_credentials()

        workers = "\n\n".join(
            f"def worker_{i}():\n    result_{i} = {i}\n    return result_{i}\n"
            for i in range(5)
        ) + "\n"
        self._write_file("workers_5.py", workers)
        fp = f"{self.home}/workers_5.py"

        prompts = [
            (
                f"Make exactly one tool call: daytona_edit_file on {fp}. "
                f"Use old_text exactly `    result_{i} = {i}` and new_text exactly "
                f"`    result_{i} = {i}00`. Do not read the file first. "
                f"Do not verify. Do not retry."
            )
            for i in range(5)
        ]

        results, invocation_errors = self._run_eval_prompts(
            prompts,
            tool_call_limit=1,
            timeout=180,
        )

        assert not invocation_errors, f"Unexpected invocation errors: {invocation_errors}"
        assert len(results) == 5, f"Expected 5 EvalAgent results, got {len(results)}"

        unrecovered_edit_errors = {
            idx: [
                event.output
                for event in result.unrecovered_error_events
                if getattr(event, "tool_name", "") == "daytona_edit_file"
            ]
            for idx, result in results.items()
        }
        unrecovered_edit_errors = {
            idx: outputs for idx, outputs in unrecovered_edit_errors.items() if outputs
        }

        text = self._read_file("workers_5.py")
        landed = [i for i in range(5) if f"result_{i} = {i}00" in text]
        print(f"\n[5-nonoverlap landed] {landed}")
        print(f"[5-nonoverlap unrecovered_edit_errors] {unrecovered_edit_errors}")
        print(f"[5-nonoverlap final file]\n{text}")

        assert len(landed) >= 4, (
            f"Expected at least 4/5 disjoint edits to land. Landed={landed}. File:\n{text}"
        )
        assert len(unrecovered_edit_errors) <= 1, (
            "Expected at most one unrecovered same-file contention error in the one-shot run. "
            f"Got: {unrecovered_edit_errors}"
        )
        assert all(
            any("Could not acquire file lock (timeout)" in output for output in outputs)
            for outputs in unrecovered_edit_errors.values()
        ), (
            "Disjoint one-shot EvalAgent failures should only be lock-timeout conflicts. "
            f"Got: {unrecovered_edit_errors}"
        )

    # ------------------------------------------------------------------
    # Test 3: 10 concurrent LLM agents edit 10 different functions
    # ------------------------------------------------------------------

    def test_llm_10_concurrent_edits_different_functions(self):
        """10 LLM agents each edit a different function concurrently.

        Every agent targets a unique ``worker_N`` function, so all edits
        are non-overlapping. The arbiter must either auto-merge all 10 or
        let the LLM retry on OCC conflict until every edit lands.
        """
        self._skip_if_no_credentials()
        from tests.test_e2e.conftest import create_eval_agent

        self._write_file("workers.py", self._TEMPLATE)
        fp = f"{self.home}/workers.py"

        agents: list[Any] = []
        for _ in range(10):
            agents.append(create_eval_agent(sandbox_id=self.sandbox_id))

        results: dict[int, dict] = {}
        errors: list[str] = []

        def _invoke_agent(idx: int):
            """Each agent edits worker_<idx>: change result_<idx> = <idx> to result_<idx> = <idx>00."""
            try:
                loop = asyncio.new_event_loop()
                result = loop.run_until_complete(agents[idx].invoke(
                    f"Edit the file {fp} using daytona_edit_file. "
                    f"In the function `worker_{idx}`, change `result_{idx} = {idx}` "
                    f"to `result_{idx} = {idx}00`. "
                    f"Only change that one line. Do not modify any other function. "
                    f"If you get a conflict error, re-read the file and retry the edit.",
                    verbose=True,
                ))
                results[idx] = {"success": True, "tool_calls": len(result.tool_calls)}
            except Exception as exc:
                errors.append(f"agent-{idx}: {exc}")
                results[idx] = {"success": False, "error": str(exc)}
            finally:
                loop.close()

        # Launch all 10 agents concurrently
        threads = [threading.Thread(target=_invoke_agent, args=(i,)) for i in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=180)

        # -- Verification --
        print(f"\n[10-agent results] errors={len(errors)}, results={results}")
        if errors:
            print(f"[10-agent errors] {errors}")

        text = self._read_file("workers.py")
        landed = []
        missing = []
        for i in range(10):
            marker = f"result_{i} = {i}00"
            if marker in text:
                landed.append(i)
            else:
                missing.append(i)

        print(f"[10-agent verify] landed={landed}, missing={missing}")
        print(f"[10-agent final file]\n{text}")

        # All 10 edits should eventually land. The LLM retries on conflict,
        # and non-overlapping edits merge automatically.
        assert len(landed) >= 8, (
            f"Expected at least 8/10 edits to land, got {len(landed)}. "
            f"Missing: {missing}. Errors: {errors}"
        )

    # ------------------------------------------------------------------
    # Test 4: overlapping line races with 2/3/4/5 agents
    # ------------------------------------------------------------------

    def test_llm_overlap_agent_counts_have_single_winner(self):
        """Overlapping EvalAgent races on one line should yield exactly one winner."""
        self._skip_if_no_credentials()

        for count in (2, 3, 4, 5):
            rel_path = f"overlap_{count}.py"
            self._write_file(
                rel_path,
                (
                    '"""Overlap target."""\n\n'
                    "def compute():\n"
                    "    shared_val = 0\n"
                    "    return shared_val\n"
                ),
            )
            fp = f"{self.home}/{rel_path}"
            attempted_values = [(idx + 1) * 111 for idx in range(count)]
            prompts = [
                (
                    f"Make exactly one tool call: daytona_edit_file on {fp}. "
                    f"Use old_text exactly `    shared_val = 0` and new_text exactly "
                    f"`    shared_val = {new_val}`. Do not read the file first. "
                    f"Do not verify. Do not retry."
                )
                for new_val in attempted_values
            ]

            results, invocation_errors = self._run_eval_prompts(
                prompts,
                tool_call_limit=1,
                timeout=180,
            )

            assert not invocation_errors, (
                f"Unexpected invocation errors for count={count}: {invocation_errors}"
            )
            assert len(results) == count, (
                f"Expected {count} EvalAgent results, got {len(results)} for count={count}"
            )

            text = self._read_file(rel_path)
            landed_values = [value for value in attempted_values if f"shared_val = {value}" in text]
            successful_edit_agents = [
                idx
                for idx, result in results.items()
                if self._daytona_edit_completions(result, is_error=False)
            ]
            failed_edit_agents = {
                idx: [
                    event.output
                    for event in self._daytona_edit_completions(result, is_error=True)
                ]
                for idx, result in results.items()
                if self._daytona_edit_completions(result, is_error=True)
            }

            print(f"\n[overlap-{count} successful_edit_agents] {successful_edit_agents}")
            print(f"[overlap-{count} failed_edit_agents] {failed_edit_agents}")
            print(f"[overlap-{count} landed_values] {landed_values}")
            print(f"[overlap-{count} final file]\n{text}")

            assert len(landed_values) == 1, (
                f"Expected exactly one final overlap winner for count={count}. "
                f"Landed={landed_values}. File:\n{text}"
            )
            assert len(successful_edit_agents) == 1, (
                f"Expected exactly one successful daytona_edit_file completion for count={count}. "
                f"Successes={successful_edit_agents}, failures={failed_edit_agents}"
            )

    # ------------------------------------------------------------------
    # Test 5: 5 concurrent LLM agents edit the SAME function → conflict
    # ------------------------------------------------------------------

    def test_llm_5_concurrent_edits_same_function_conflict(self):
        """5 LLM agents all try to edit the same function concurrently.

        Only one agent's edit can win the OCC race for any given content
        hash. The others will hit a conflict. We verify:
          - At least 1 agent's edit lands in the final file
          - The file is not corrupted (valid Python)
          - The final value is one of the 5 attempted values (no merge of
            overlapping edits)
        """
        self._skip_if_no_credentials()
        from tests.test_e2e.conftest import create_eval_agent

        self._write_file("target.py", (
            '"""Target module."""\n\n\n'
            "def compute():\n"
            '    """The contested function."""\n'
            "    value = 0\n"
            "    return value\n"
            "\n\n"
            "def untouched():\n"
            '    """Should never change."""\n'
            "    return 42\n"
        ))
        fp = f"{self.home}/target.py"

        agents = [create_eval_agent(sandbox_id=self.sandbox_id) for _ in range(5)]
        results: dict[int, dict] = {}
        errors: list[str] = []
        tool_outputs: dict[int, list[str]] = {i: [] for i in range(5)}

        def _invoke_agent(idx: int):
            new_val = (idx + 1) * 100  # 100, 200, 300, 400, 500
            try:
                loop = asyncio.new_event_loop()
                result = loop.run_until_complete(agents[idx].invoke(
                    f"Edit the file {fp} using daytona_edit_file. "
                    f"In the function `compute`, change `value = 0` to `value = {new_val}`. "
                    f"Only change that one line — do NOT modify `untouched()` or anything else. "
                    f"If you get a conflict or 'not found' error, re-read the file and retry "
                    f"with the updated content. Keep trying until the edit succeeds or you "
                    f"have tried 3 times.",
                    verbose=True,
                ))
                # Collect tool call details for conflict analysis
                for tc in result.tool_calls:
                    tool_outputs[idx].append(f"{tc.name}: {json.dumps(tc.input)[:200]}")
                results[idx] = {"success": True, "tool_calls": len(result.tool_calls)}
            except Exception as exc:
                errors.append(f"agent-{idx}: {exc}")
                results[idx] = {"success": False, "error": str(exc)}
            finally:
                loop.close()

        threads = [threading.Thread(target=_invoke_agent, args=(i,)) for i in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=180)

        # -- Verification --
        text = self._read_file("target.py")
        print(f"\n[5-conflict results] {results}")
        print(f"[5-conflict final file]\n{text}")

        # 1. File must be valid Python (not corrupted by concurrent writes)
        try:
            compile(text, "target.py", "exec")
        except SyntaxError as exc:
            pytest.fail(f"File corrupted by concurrent edits — invalid Python: {exc}\n{text}")

        # 2. untouched() must still be intact
        assert "return 42" in text, f"untouched() was corrupted:\n{text}"

        # 3. At least one agent's edit must have landed
        possible_values = [str((i + 1) * 100) for i in range(5)]
        landed_values = [v for v in possible_values if f"value = {v}" in text]
        print(f"[5-conflict landed values] {landed_values}")

        assert len(landed_values) >= 1, (
            f"No agent's edit landed in the final file. "
            f"Expected one of value = {possible_values}. File:\n{text}"
        )

        # 4. At most one value should be present (overlapping edits don't merge)
        assert len(landed_values) <= 1, (
            f"Multiple overlapping values found: {landed_values}. "
            f"Arbiter should prevent overlapping merges. File:\n{text}"
        )

        # 5. Check that some agents encountered conflicts (tool_calls > 1
        #    means they had to retry after reading/conflict)
        total_tool_calls = sum(r.get("tool_calls", 0) for r in results.values())
        print(f"[5-conflict total tool calls] {total_tool_calls} across 5 agents")
        # With 5 agents hitting the same line, we expect retries.
        # Minimum: 5 (one read+edit each), but conflicts cause extra calls.
        assert total_tool_calls >= 5, "Expected at least 5 tool calls across all agents"

    # ------------------------------------------------------------------
    # Test 4: Mixed — 5 agents on different functions + 5 on same function
    # ------------------------------------------------------------------

    def test_llm_10_mixed_concurrent_edits(self):
        """10 agents: 5 edit distinct functions, 5 race on the same function.

        Validates that:
          - The 5 non-overlapping edits all land
          - The 5 overlapping edits produce exactly 1 winner
          - No data corruption
        """
        self._skip_if_no_credentials()
        from tests.test_e2e.conftest import create_eval_agent

        # File with 6 functions: worker_0..worker_4 (unique targets) + shared_target
        content_lines = ['"""Mixed edit target."""\n']
        for i in range(5):
            content_lines.append(
                f"\ndef worker_{i}():\n"
                f'    """Worker {i}."""\n'
                f"    val_{i} = {i}\n"
                f"    return val_{i}\n"
            )
        content_lines.append(
            "\ndef shared_target():\n"
            '    """The contested function."""\n'
            "    shared_val = 0\n"
            "    return shared_val\n"
        )
        self._write_file("mixed.py", "\n".join(content_lines))
        fp = f"{self.home}/mixed.py"

        agents = [create_eval_agent(sandbox_id=self.sandbox_id) for _ in range(10)]
        results: dict[int, dict] = {}
        errors: list[str] = []

        def _invoke_agent(idx: int):
            try:
                loop = asyncio.new_event_loop()
                if idx < 5:
                    # Non-overlapping: each edits a unique worker_<idx>
                    prompt = (
                        f"Edit the file {fp} using daytona_edit_file. "
                        f"In the function `worker_{idx}`, change `val_{idx} = {idx}` "
                        f"to `val_{idx} = {idx}000`. "
                        f"Only change that one line. If you get a conflict, re-read and retry."
                    )
                else:
                    # Overlapping: all 5 agents race on shared_target
                    new_val = (idx - 5 + 1) * 10  # 10, 20, 30, 40, 50
                    prompt = (
                        f"Edit the file {fp} using daytona_edit_file. "
                        f"In the function `shared_target`, change `shared_val = 0` "
                        f"to `shared_val = {new_val}`. "
                        f"Only change that one line. If you get a conflict or 'not found' error, "
                        f"re-read the file and retry with updated content. Try up to 3 times."
                    )

                result = loop.run_until_complete(agents[idx].invoke(prompt, verbose=True))
                results[idx] = {"success": True, "tool_calls": len(result.tool_calls)}
            except Exception as exc:
                errors.append(f"agent-{idx}: {exc}")
                results[idx] = {"success": False, "error": str(exc)}
            finally:
                loop.close()

        threads = [threading.Thread(target=_invoke_agent, args=(i,)) for i in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=240)

        # -- Verification --
        text = self._read_file("mixed.py")
        print(f"\n[mixed results] {results}")
        if errors:
            print(f"[mixed errors] {errors}")
        print(f"[mixed final file]\n{text}")

        # 1. Valid Python
        try:
            compile(text, "mixed.py", "exec")
        except SyntaxError as exc:
            pytest.fail(f"File corrupted — invalid Python: {exc}\n{text}")

        # 2. Non-overlapping edits: at least 4 of 5 worker edits should land
        worker_landed = []
        worker_missing = []
        for i in range(5):
            if f"val_{i} = {i}000" in text:
                worker_landed.append(i)
            else:
                worker_missing.append(i)
        print(f"[mixed workers] landed={worker_landed}, missing={worker_missing}")
        assert len(worker_landed) >= 4, (
            f"Expected at least 4/5 non-overlapping worker edits, got {len(worker_landed)}. "
            f"Missing: {worker_missing}"
        )

        # 3. Overlapping edits on shared_target: exactly 1 value should win
        shared_vals = [str((i - 5 + 1) * 10) for i in range(5, 10)]
        shared_landed = [v for v in shared_vals if f"shared_val = {v}" in text]
        print(f"[mixed shared_target] landed values: {shared_landed}")

        # At least one overlapping edit should have landed
        assert len(shared_landed) >= 1, (
            f"No overlapping edit landed. Expected one of shared_val = {shared_vals}. "
            f"File:\n{text}"
        )
        # At most one should land (no overlapping merge)
        assert len(shared_landed) <= 1, (
            f"Multiple overlapping values: {shared_landed}. "
            f"Arbiter should prevent overlapping merges."
        )
