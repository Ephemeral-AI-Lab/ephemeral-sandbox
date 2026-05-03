"""Phase 3.5 — SymbolIndex parity between in-memory and SQLite-backed paths.

Confirms that injecting a SQLite ``IndexStore`` into ``SymbolIndex`` does
not change query, refresh, or remove semantics. The drift-guard test for the
3.5 cleanup that retires the orchestrator-side pickle snapshot.
"""

from __future__ import annotations

from pathlib import Path

from sandbox.code_intelligence.daemon.storage import IndexStore
from sandbox.code_intelligence.indexing.symbol_index import SymbolIndex


def _seed_workspace(root: Path) -> dict[str, str]:
    """Write a tiny three-file Python workspace under ``root``."""
    files = {
        "alpha.py": "def alpha_one():\n    return 1\n\n\ndef alpha_two():\n    return 2\n",
        "beta.py": "class BetaThing:\n    def beta_method(self):\n        return 'b'\n",
        "gamma/__init__.py": "GAMMA_CONST = 42\n\n\ndef gamma_func():\n    return GAMMA_CONST\n",
    }
    for rel, content in files.items():
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)
    return files


def _build_index(workspace: Path, *, persistence: object | None = None) -> SymbolIndex:
    si = SymbolIndex(str(workspace), persistence=persistence)
    assert si.ensure_built(wait=True), "SymbolIndex did not build"
    return si


def test_query_parity_between_inmemory_and_sqlite(tmp_path: Path) -> None:
    workspace = tmp_path / "ws"
    workspace.mkdir()
    _seed_workspace(workspace)

    in_memory = _build_index(workspace, persistence=None)

    sqlite_state = tmp_path / "state"
    store = IndexStore(state_dir_path=sqlite_state)
    try:
        sqlite_idx = _build_index(workspace, persistence=store)

        for needle in ("alpha", "beta", "gamma", "method", "Const"):
            mem_results = sorted(s.name for s in in_memory.find(needle))
            sql_results = sorted(s.name for s in sqlite_idx.find(needle))
            assert mem_results == sql_results, (needle, mem_results, sql_results)
    finally:
        store.close()


def test_refresh_parity(tmp_path: Path) -> None:
    workspace = tmp_path / "ws"
    workspace.mkdir()
    _seed_workspace(workspace)

    in_memory = _build_index(workspace, persistence=None)

    sqlite_state = tmp_path / "state"
    store = IndexStore(state_dir_path=sqlite_state)
    try:
        sqlite_idx = _build_index(workspace, persistence=store)

        target = workspace / "alpha.py"
        target.write_text("def renamed_alpha():\n    return 'r'\n")
        in_memory.refresh(str(target))
        sqlite_idx.refresh(str(target))

        mem = sorted(s.name for s in in_memory.find("alpha"))
        sql = sorted(s.name for s in sqlite_idx.find("alpha"))
        assert mem == sql == ["renamed_alpha"]

        # Persistence saw the update too.
        store_syms = sorted(s.name for s in store.file_symbols(str(target)))
        assert store_syms == ["renamed_alpha"]
    finally:
        store.close()


def test_remove_parity(tmp_path: Path) -> None:
    workspace = tmp_path / "ws"
    workspace.mkdir()
    _seed_workspace(workspace)

    in_memory = _build_index(workspace, persistence=None)

    sqlite_state = tmp_path / "state"
    store = IndexStore(state_dir_path=sqlite_state)
    try:
        sqlite_idx = _build_index(workspace, persistence=store)

        target = workspace / "beta.py"
        in_memory.remove(str(target))
        sqlite_idx.remove(str(target))

        assert not in_memory.find("BetaThing")
        assert not sqlite_idx.find("BetaThing")
        assert store.file_symbols(str(target)) == []
    finally:
        store.close()


def test_indexed_paths_parity(tmp_path: Path) -> None:
    workspace = tmp_path / "ws"
    workspace.mkdir()
    _seed_workspace(workspace)

    in_memory = _build_index(workspace, persistence=None)
    sqlite_state = tmp_path / "state"
    store = IndexStore(state_dir_path=sqlite_state)
    try:
        sqlite_idx = _build_index(workspace, persistence=store)
        assert in_memory.indexed_paths() == sqlite_idx.indexed_paths()
        assert in_memory.size == sqlite_idx.size
    finally:
        store.close()


def test_persistence_kwarg_optional() -> None:
    """Default behavior (persistence=None) preserves today's contract."""
    si = SymbolIndex("/nonexistent")
    assert si._persistence is None  # noqa: SLF001 - verifying the default


def test_index_store_round_trip_via_symbol_index(tmp_path: Path) -> None:
    """An IndexStore reopened from disk keeps the symbols added through SymbolIndex."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    _seed_workspace(workspace)

    sqlite_state = tmp_path / "state"
    store = IndexStore(state_dir_path=sqlite_state)
    try:
        si = _build_index(workspace, persistence=store)
        # Verify the in-memory build mirrored to disk.
        assert sorted(store.indexed_paths()) == sorted(si.indexed_paths())
        baseline = sorted(s.name for s in si.find("alpha"))
    finally:
        store.close()

    # Reopen from disk — symbols persist across IndexStore instances.
    store2 = IndexStore(state_dir_path=sqlite_state)
    try:
        round_tripped = sorted(
            sym.name
            for path in store2.indexed_paths()
            for sym in store2.file_symbols(path)
            if "alpha" in sym.name.lower()
        )
        assert baseline == round_tripped
    finally:
        store2.close()


def test_symbol_index_hydrates_from_existing_index_store(tmp_path: Path) -> None:
    """A daemon restart can serve the persisted index before rebuilding files."""
    workspace = tmp_path / "ws"
    workspace.mkdir()
    _seed_workspace(workspace)

    sqlite_state = tmp_path / "state"
    store = IndexStore(state_dir_path=sqlite_state)
    try:
        built = _build_index(workspace, persistence=store)
        baseline = sorted(s.name for s in built.find("alpha"))
    finally:
        store.close()

    store2 = IndexStore(state_dir_path=sqlite_state)
    try:
        hydrated = SymbolIndex(str(workspace), persistence=store2)
        assert hydrated.is_built is True
        assert sorted(s.name for s in hydrated.find("alpha")) == baseline
        assert sorted(hydrated.indexed_paths()) == sorted(store2.indexed_paths())
        stored_size = sum(
            len(store2.file_symbols(path)) for path in store2.indexed_paths()
        )
        assert hydrated.size == stored_size
    finally:
        store2.close()
