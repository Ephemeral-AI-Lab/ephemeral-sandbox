"""Benchmark: symbol index cold start speed on a real SWE-EVO sandbox.

Usage:
    PYTHONPATH=src python tests/test_benchmarks/bench_symbol_index_speed.py [instance_id]

Uses the SWE-EVO snapshot infrastructure to get a sandbox with the
dask repo already checked out, then measures the symbol index build.
"""
from __future__ import annotations

import asyncio
import logging
import sys
import time

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s")
logger = logging.getLogger("bench")


async def main() -> None:
    from benchmarks.sweevo.dataset import select_sweevo_instance
    from benchmarks.sweevo.sandbox import create_sweevo_test_sandbox
    from sandbox.lifecycle import shutdown_cached_client
    from code_intelligence.routing.service import CodeIntelligenceService

    instance_id = sys.argv[1] if len(sys.argv) > 1 else "pydantic__pydantic_v2.6.0b1_v2.6.0"

    instance = select_sweevo_instance(
        instance_id=instance_id,
    )
    logger.info("Instance: %s repo=%s", instance.instance_id, instance.repo)

    # Step 1: Create sandbox via sweevo infra (handles snapshot/image)
    logger.info("Creating SWE-EVO sandbox...")
    t0 = time.time()
    try:
        sandbox_result = await create_sweevo_test_sandbox(
            instance,
            register_snapshot=True,
            cpu=2,
            disk=10,
        )
    except Exception:
        logger.exception("Failed to create sandbox")
        return
    sandbox_id = sandbox_result["sandbox_id"]
    logger.info("Sandbox ready: %s (%.1fs)", sandbox_id, time.time() - t0)

    # Use SYNC sandbox handle (same as inject_code_intelligence does)
    from sandbox.service import SandboxService as SvcClass
    sync_svc = SvcClass()
    sandbox = sync_svc.get_sandbox_object(sandbox_id)
    workspace_root = "/testbed"

    try:
        # Verify workspace exists via sync handle
        resp = sandbox.process.exec(
            "bash -c 'find /testbed -maxdepth 1 -name \"*.py\" -o -type d | head -20'",
            timeout=15,
        )
        logger.info("Workspace contents: %s", (resp.result or "")[:300])

        resp2 = sandbox.process.exec(
            "bash -c 'find /testbed -name \"*.py\" -not -path \"*/.git/*\" -not -path \"*/node_modules/*\" | wc -l'",
            timeout=30,
        )
        py_count = (resp2.result or "").strip()
        logger.info("Python files: %s", py_count)

        # Step 2: Build symbol index (cold start)
        logger.info("=" * 60)
        logger.info("COLD START: Building symbol index for %s (%s files)", workspace_root, py_count)
        logger.info("=" * 60)

        ci_svc = CodeIntelligenceService(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
            sandbox=sandbox,
        )

        t_build_start = time.time()
        ready = ci_svc.symbol_index.ensure_built(wait=True, timeout=300.0)
        t_build_end = time.time()
        build_duration = t_build_end - t_build_start

        if ready:
            logger.info(
                "RESULT: %d files, %d symbols in %.2fs",
                ci_svc.symbol_index.indexed_files,
                ci_svc.symbol_index.size,
                build_duration,
            )
        else:
            logger.error("FAILED: build timed out after %.1fs", build_duration)

        # Step 3: Query benchmark
        for query in ["DataFrame", "compute", "delayed", "Array", "BaseModel"]:
            t_q = time.time()
            results = ci_svc.symbol_index.find(query)
            logger.info(
                "Query '%s': %d results in %.3fs",
                query, len(results), time.time() - t_q,
            )

        # Step 4: Refresh benchmark (simulates file edit)
        logger.info("=" * 60)
        logger.info("REFRESH: Simulating file edits")
        logger.info("=" * 60)

        # 4a: Refresh with content provided (in-memory, no HTTP)
        old_size = ci_svc.symbol_index.size
        new_content = (
            "class NewModel:\n    name: str\n    value: int\n\n"
            "def new_function(x: int) -> str:\n    return str(x)\n"
        )
        t_refresh = time.time()
        gen = ci_svc.symbol_index.refresh("/testbed/pydantic/main.py", content=new_content)
        refresh_with_content = time.time() - t_refresh

        # Validate: new symbols should be findable
        new_model_hits = ci_svc.symbol_index.find("NewModel")
        new_func_hits = ci_svc.symbol_index.find("new_function")
        assert len(new_model_hits) >= 1, f"NewModel not found after refresh! got {new_model_hits}"
        assert len(new_func_hits) >= 1, f"new_function not found after refresh! got {new_func_hits}"
        assert new_model_hits[0].file_path == "/testbed/pydantic/main.py"
        logger.info(
            "Refresh WITH content: %.4fs (gen=%d, NewModel=%d hits, new_function=%d hits) ✓",
            refresh_with_content, gen, len(new_model_hits), len(new_func_hits),
        )

        # 4b: Refresh without content (needs HTTP download of actual file)
        t_refresh2 = time.time()
        gen2 = ci_svc.symbol_index.refresh("/testbed/pydantic/main.py")
        refresh_no_content = time.time() - t_refresh2

        # Validate: our fake symbols should be gone from that file, replaced by real ones
        file_syms = ci_svc.symbol_index.file_symbols("/testbed/pydantic/main.py")
        file_sym_names = [s.name for s in file_syms]
        assert "new_function" not in file_sym_names, f"Fake new_function should be gone, got {file_sym_names}"
        assert len(file_syms) > 0, "Real file should have symbols after re-download"
        logger.info(
            "Refresh WITHOUT content (1 HTTP): %.4fs (gen=%d, real symbols=%d) ✓",
            refresh_no_content, gen2, len(file_syms),
        )

        # 4c: Batch of 10 refreshes with content
        t_batch = time.time()
        for i in range(10):
            ci_svc.symbol_index.refresh(
                f"/testbed/pydantic/fake_{i}.py",
                content=f"def func_{i}(): pass\nclass Cls_{i}: pass\n",
            )
        refresh_batch = time.time() - t_batch

        # Validate batch
        for i in range(10):
            hits = ci_svc.symbol_index.find(f"func_{i}")
            assert len(hits) >= 1, f"func_{i} not found after batch refresh"
        logger.info("10x refresh WITH content: %.4fs (%.4fs avg) ✓", refresh_batch, refresh_batch / 10)

        # Step 5: TreeCache stats
        if hasattr(ci_svc, "tree_cache"):
            logger.info("TreeCache: %s", ci_svc.tree_cache.stats)

        logger.info("=" * 60)
        logger.info("BUILD TIME: %.2fs (%s files)", build_duration, py_count)
        logger.info("Refresh with content: %.4fs", refresh_with_content)
        logger.info("Refresh without content: %.4fs", refresh_no_content)
        logger.info("=" * 60)

    finally:
        logger.info("Deleting sandbox %s...", sandbox_id)
        try:
            sync_svc.delete_sandbox(sandbox_id)
        except Exception:
            logger.warning("Delete failed", exc_info=True)
        try:
            shutdown_cached_client()
        except Exception:
            pass


if __name__ == "__main__":
    asyncio.run(main())
