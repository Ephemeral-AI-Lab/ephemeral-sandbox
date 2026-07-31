from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("build-mpla-publication-fixture-cache")


def load_cache_builder_module():
    loader = importlib.machinery.SourceFileLoader("mpla_fixture_cache", str(SCRIPT))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    if spec is None:
        raise RuntimeError("fixture-cache loader did not produce a module spec")
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class FixtureCacheBuilderTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.cache_builder = load_cache_builder_module()

    def test_failure_output_retains_cause_and_terminal_context(self) -> None:
        source = "cause:" + "a" * 2000 + ":terminal"

        bounded = self.cache_builder.bounded_failure_output(source)

        self.assertTrue(bounded.startswith("cause:"))
        self.assertTrue(bounded.endswith(":terminal"))
        self.assertIn("characters omitted", bounded)
        self.assertLess(len(bounded), len(source))

    def test_cache_build_uses_the_persistent_sparse_fixture_identity(self) -> None:
        self.assertIn(
            "/s4-chain-sparse-v1/", self.cache_builder.PREPARED_FIXTURE_TOOL_ROOT
        )
        self.assertIn(
            "fixture-builder-stage-sparse-v1",
            str(self.cache_builder.STAGING_CACHE_ROOT),
        )
        with mock.patch.object(sys, "argv", ["fixture-cache-builder"]):
            args = self.cache_builder.parse_args()
            self.assertEqual(args.gateway_socket, "127.0.0.1:7902")
            self.assertEqual(args.timeout_seconds, 30)
            self.assertIsNone(args.evidence_file)

    def test_cold_phase_receipt_is_create_new(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            receipt_path = pathlib.Path(temporary) / "fixture-cache-construction.json"
            receipt = {
                "phase": "F0-COLD",
                "cold_build_outer_elapsed_ms": 1_403.286,
                "payload_bytes_were_copied": False,
            }

            self.cache_builder.write_new_json(receipt_path, receipt)

            self.assertEqual(json.loads(receipt_path.read_text()), receipt)
            with self.assertRaises(FileExistsError):
                self.cache_builder.write_new_json(receipt_path, receipt)

    def builder_result(self) -> dict[str, object]:
        return {
            "fixture_profile": self.cache_builder.PREPARED_FIXTURE_PROFILE,
            "manifest_path": self.cache_builder.PREPARED_FIXTURE_MANIFEST,
            "chain_depth": self.cache_builder.PREPARED_FIXTURE_CHAIN_DEPTH,
            "logical_bytes": self.cache_builder.PREPARED_FIXTURE_LOGICAL_BYTES,
            "allocation_count": self.cache_builder.PREPARED_FIXTURE_ALLOCATION_COUNT,
            "allocated_bytes": 0,
            "payload_bytes_read": 0,
            "payload_bytes_copied": 0,
            "builder_elapsed_ns": 1,
        }

    def test_validate_builder_result_requires_fixed_sparse_fixture_attestation(self) -> None:
        result = self.builder_result()

        self.assertEqual(self.cache_builder.validate_builder_result(result), result)

    def test_validate_builder_result_rejects_malformed_or_wrong_attestation(self) -> None:
        wrong_values = {
            "fixture_profile": "retired-v9",
            "manifest_path": "/unexpected/manifest.json",
            "chain_depth": 7,
            "logical_bytes": 8 * 1024 * 1024 * 1024 - 1,
            "allocation_count": 7,
            "allocated_bytes": 1,
            "payload_bytes_read": 1,
            "payload_bytes_copied": 1,
            "builder_elapsed_ns": 0,
        }
        with self.subTest("non-object"):
            with self.assertRaisesRegex(
                self.cache_builder.CacheBuildError, "not an object"
            ):
                self.cache_builder.validate_builder_result([])
        for field, wrong_value in wrong_values.items():
            with self.subTest(field=field):
                result = self.builder_result()
                result[field] = wrong_value
                with self.assertRaises(self.cache_builder.CacheBuildError):
                    self.cache_builder.validate_builder_result(result)

    def test_orchestration_overhead_rejects_impossible_timing_nesting(self) -> None:
        self.assertEqual(
            self.cache_builder.checked_orchestration_overhead_ns(10, 4),
            6,
        )
        with self.assertRaisesRegex(
            self.cache_builder.CacheBuildError,
            "service elapsed time exceeds",
        ):
            self.cache_builder.checked_orchestration_overhead_ns(4, 10)

    def test_main_writes_create_new_receipt_only_after_valid_builder_result(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_path = pathlib.Path(temporary) / "fixture-cache-construction.json"
            args = self.cache_builder.argparse.Namespace(
                gateway_socket="127.0.0.1:7902",
                build_commit=self.cache_builder.BUILD_COMMIT,
                timeout_seconds=30,
                evidence_file=evidence_path,
            )
            result_line = self.cache_builder.RESULT_PREFIX + json.dumps(self.builder_result())
            with (
                mock.patch.object(self.cache_builder, "parse_args", return_value=args),
                mock.patch.object(self.cache_builder, "stage_tools", return_value=pathlib.Path("/stage")),
                mock.patch.object(self.cache_builder, "create_sandbox", return_value="eos-candidate") as create,
                mock.patch.object(
                    self.cache_builder,
                    "runtime",
                    side_effect=[{"workspace_session_id": "session-1"}, {"status": "running"}],
                ) as runtime,
                mock.patch.object(self.cache_builder, "wait_for_builder", return_value=result_line) as wait,
                mock.patch.object(self.cache_builder, "cleanup", return_value=[]) as cleanup,
            ):
                self.assertEqual(self.cache_builder.main(), 0)

            receipt = json.loads(evidence_path.read_text())
            self.assertEqual(receipt["service_result"], self.builder_result())
            self.assertFalse(receipt["payload_bytes_were_copied"])
            self.assertEqual(create.call_count, 1)
            self.assertEqual(wait.call_count, 1)
            self.assertEqual(runtime.call_count, 2)
            cleanup.assert_called_once_with(
                args.gateway_socket, "eos-candidate", "session-1", ["eos-candidate"]
            )

    def test_main_cleans_up_after_builder_failure_or_timeout(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            args = self.cache_builder.argparse.Namespace(
                gateway_socket="127.0.0.1:7902",
                build_commit=self.cache_builder.BUILD_COMMIT,
                timeout_seconds=30,
                evidence_file=pathlib.Path(temporary) / "fixture-cache-construction.json",
            )
            for failure in (
                self.cache_builder.CacheBuildError("builder rejected cache"),
                self.cache_builder.subprocess.TimeoutExpired("builder", 30),
            ):
                with (
                    self.subTest(failure=type(failure).__name__),
                    mock.patch.object(self.cache_builder, "parse_args", return_value=args),
                    mock.patch.object(self.cache_builder, "stage_tools", return_value=pathlib.Path("/stage")),
                    mock.patch.object(self.cache_builder, "create_sandbox", return_value="eos-candidate"),
                    mock.patch.object(
                        self.cache_builder,
                        "runtime",
                        side_effect=[{"workspace_session_id": "session-1"}, {"status": "running"}],
                    ),
                    mock.patch.object(self.cache_builder, "wait_for_builder", side_effect=failure),
                    mock.patch.object(self.cache_builder, "cleanup", return_value=[]) as cleanup,
                ):
                    with self.assertRaises(type(failure)):
                        self.cache_builder.main()
                    cleanup.assert_called_once_with(
                        args.gateway_socket,
                        "eos-candidate",
                        "session-1",
                        ["eos-candidate"],
                    )
                    self.assertFalse(args.evidence_file.exists())

    def test_main_rejects_cleanup_failure_without_publishing_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_path = pathlib.Path(temporary) / "fixture-cache-construction.json"
            args = self.cache_builder.argparse.Namespace(
                gateway_socket="127.0.0.1:7902",
                build_commit=self.cache_builder.BUILD_COMMIT,
                timeout_seconds=30,
                evidence_file=evidence_path,
            )
            result_line = self.cache_builder.RESULT_PREFIX + json.dumps(
                self.builder_result()
            )
            with (
                mock.patch.object(self.cache_builder, "parse_args", return_value=args),
                mock.patch.object(
                    self.cache_builder,
                    "stage_tools",
                    return_value=pathlib.Path("/stage"),
                ),
                mock.patch.object(
                    self.cache_builder,
                    "create_sandbox",
                    return_value="eos-candidate",
                ),
                mock.patch.object(
                    self.cache_builder,
                    "runtime",
                    side_effect=[
                        {"workspace_session_id": "session-1"},
                        {"status": "running"},
                    ],
                ),
                mock.patch.object(
                    self.cache_builder,
                    "wait_for_builder",
                    return_value=result_line,
                ),
                mock.patch.object(
                    self.cache_builder,
                    "cleanup",
                    return_value=["sandbox eos-candidate cleanup: refused"],
                ),
            ):
                with self.assertRaisesRegex(
                    self.cache_builder.CacheBuildError,
                    "fixture builder cleanup failed",
                ):
                    self.cache_builder.main()

            self.assertFalse(evidence_path.exists())

    def test_builder_stage_requires_the_independent_oracle(self) -> None:
        self.assertEqual(
            self.cache_builder.TOOL_SOURCE_PACKAGES[:2],
            ("sandbox-runtime-mpla-poc", "sandbox-runtime-mpla-poc"),
        )
        source = SCRIPT.read_text()
        self.assertIn('require_regular(linux / "mpla-poc-oracle")', source)
        self.assertIn("*tool_artifacts[:4]", source)

    def test_cold_build_has_one_container_and_separate_acceptance_clocks(self) -> None:
        source = SCRIPT.read_text()

        self.assertEqual(
            source.count("create_sandbox(args.gateway_socket, stage_root,"),
            1,
        )
        self.assertIn("coordinator = candidate", source)
        self.assertIn(
            '"cold_build_under_5s": cold_build_elapsed_ns < 5_000_000_000',
            source,
        )
        self.assertIn('"docker_setup_elapsed_ms":', source)
        self.assertIn('"artifact_staging_elapsed_ms":', source)
        self.assertEqual(
            self.cache_builder.SANDBOX_CREATE_TIMEOUT_SECONDS,
            120,
        )
        self.assertNotIn("\n        600,\n", source)
        self.assertLess(
            source.index("docker_setup_elapsed_ns ="),
            source.index("cold_build_started ="),
        )

    def test_cache_warm_rejects_a_stale_builder_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            source = root / "source.rs"
            artifact = root / "mpla-speed-poc-v1"
            source.write_text("source")
            artifact.write_text("artifact")
            os.utime(source, ns=(2_000, 2_000))
            os.utime(artifact, ns=(1_000, 1_000))

            with self.assertRaisesRegex(
                self.cache_builder.CacheBuildError,
                "builder artifact is older than its source inputs",
            ):
                self.cache_builder.require_fresh_builder_tools((artifact,), (source,))

    def test_dep_info_excludes_an_unrelated_newer_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            source = root / "listed.rs"
            unrelated = root / "unrelated.rs"
            artifact = root / "mpla-poc-oracle"
            dep_info = artifact.with_suffix(".d")
            source.write_text("listed")
            unrelated.write_text("unrelated")
            artifact.write_text("artifact")
            dep_info.write_text(f"{artifact}: {source}\n")
            os.utime(source, ns=(1_000, 1_000))
            os.utime(unrelated, ns=(3_000, 3_000))
            os.utime(artifact, ns=(2_000, 2_000))

            with mock.patch.object(self.cache_builder, "COMMON_TOOL_SOURCE_INPUTS", ()):
                inputs = self.cache_builder.cargo_dep_info_source_inputs(artifact)

                self.assertIn(source, inputs)
                self.assertNotIn(unrelated, inputs)
                self.cache_builder.require_fresh_builder_tools((artifact,), inputs)

    def test_cli_freshness_uses_its_local_dependency_closure(self) -> None:
        roots = self.cache_builder.cargo_workspace_dependency_source_roots(
            ("sandbox-cli",)
        )["sandbox-cli"]
        inputs = self.cache_builder.scorecard_tool_source_inputs(roots)

        self.assertIn(self.cache_builder.REPO / "crates" / "sandbox-cli" / "Cargo.toml", inputs)
        self.assertNotIn(
            self.cache_builder.REPO / "crates" / "sandbox-provider-docker" / "Cargo.toml",
            inputs,
        )


if __name__ == "__main__":
    unittest.main()
