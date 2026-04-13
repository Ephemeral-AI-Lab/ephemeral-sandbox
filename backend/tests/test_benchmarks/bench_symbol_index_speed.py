"""Benchmark: symbol index cold start speed on a real sandbox.

Usage:
    PYTHONPATH=src python tests/test_benchmarks/bench_symbol_index_speed.py

Creates a plain sandbox, clones dask/dask at tag 2023.4.0, then
measures the symbol index build time on ~300 Python files.
"""
from __future__ import annotations

import asyncio
import logging
import time

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s")
logger = logging.getLogger("bench")


def main() -> None:
    from sandbox.service import SandboxService
    from sandbox.workspace import inject_code_intelligence
    from code_intelligence.routing.service import CodeIntelligenceService
    from unittest.mock import MagicMock

    svc = SandboxService()

    # Step 1: Create sandbox
    logger.info("Creating sandbox...")
    t0 = time.time()
    result = svc.create_sandbox(
        name=f"bench-dask-{int(time.time())}",
        language="python",
        labels={"purpose": "bench-symbol-index"},
    )
    sandbox_id = result["id"]
    sandbox = svc.get_sandbox_object(sandbox_id)
    logger.info("Sandbox ready: %s (%.1fs)", sandbox_id, time.time() - t0)

    try:
        # Step 2: Clone dask/dask
        logger.info("Cloning dask/dask (2023.4.0, depth=1)...")
        t_clone = time.time()
        resp = sandbox.process.exec(
            "git clone --depth 1 --branch 2023.4.0 https://github.com/dask/dask.git /home/daytona/dask",
            timeout=120,
        )
        logger.info("Clone: exit=%s (%.1fs)", resp.exit_code, time.time() - t_clone)

        # Count files
        resp2 = sandbox.process.exec("find /home/daytona/dask -name '*.py' | wc -l", timeout=30)
        logger.info("Python files in dask: %s", (resp2.result or "").strip())

        workspace_root = "/home/daytona/dask"

        # Step 3: Build symbol index (cold start)
        logger.info("=" * 60)
        logger.info("COLD START: Building symbol index for %s", workspace_root)
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

        # Step 4: Query benchmark
        for query in ["DataFrame", "compute", "delayed", "Array"]:
            t_q = time.time()
            results = ci_svc.symbol_index.find(query)
            logger.info(
                "Query '%s': %d results in %.3fs",
                query, len(results), time.time() - t_q,
            )

        # Step 5: TreeCache stats
        if hasattr(ci_svc, "tree_cache"):
            logger.info("TreeCache: %s", ci_svc.tree_cache.stats)

        logger.info("=" * 60)
        logger.info("BUILD TIME: %.2fs", build_duration)
        logger.info("=" * 60)

    finally:
        logger.info("Deleting sandbox %s...", sandbox_id)
        try:
            svc.delete_sandbox(sandbox_id)
        except Exception:
            logger.warning("Delete failed", exc_info=True)


if __name__ == "__main__":
    main()
