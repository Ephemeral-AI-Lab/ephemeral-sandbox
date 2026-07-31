from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import pathlib
import tempfile
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("run-mpla-booster-scorecard")


def load_scorecard_module():
    loader = importlib.machinery.SourceFileLoader("mpla_booster_scorecard", str(SCRIPT))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    if spec is None:
        raise RuntimeError("scorecard loader did not produce a module spec")
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class BoosterScorecardHarnessTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.scorecard = load_scorecard_module()

    def create_sealed_phase_root(
        self,
        parent: pathlib.Path,
        case: str,
        *,
        receipt_status: str = "PASS",
        include_warm_receipt: bool = True,
        warm_receipt_overrides: dict | None = None,
    ) -> pathlib.Path:
        root = parent / case
        root.mkdir()
        run_id = f"phase-root-test-{case}"
        result = {
            "schema_version": 1,
            "kind": self.scorecard.CASE_RESULT_KINDS[case],
        }
        raw_paths = [
            root / "cases" / gate / "raw-result.json"
            for gate in self.scorecard.CASE_GATES[case]
        ] or [root / "cases" / case / "raw-result.json"]
        for path in raw_paths:
            path.parent.mkdir(parents=True, exist_ok=True)
            self.scorecard.write_new_json(path, result)
        self.scorecard.write_new_json(
            root / "source.json",
            {"commit": self.scorecard.BUILD_COMMIT},
        )
        self.scorecard.write_new_json(
            root / "environment.json",
            {
                "run_id": run_id,
                "case": case,
                "build_commit": self.scorecard.BUILD_COMMIT,
                "created_utc": (
                    "2026-07-30T00:"
                    f"{self.scorecard.SCORECARD_CASES.index(case):02}:00+00:00"
                ),
            },
        )
        self.scorecard.write_new_json(
            root / "decision.json",
            {
                "run_id": run_id,
                "phase": case,
                "status": receipt_status,
            },
        )
        self.scorecard.write_new_json(
            root / "phase-receipt.json",
            {
                "run_id": run_id,
                "phase": case,
                "runner": self.scorecard.CASE_RUNNERS[case],
                "status": receipt_status,
                "cap_pass": receipt_status == "PASS",
                "cleanup_pass": receipt_status == "PASS",
                "deadline_carryover_seconds": 0,
            },
        )
        self.scorecard.write_new_json(
            root / "phase-declaration.json",
            {
                "run_id": run_id,
                "phase": case,
                "runner": self.scorecard.CASE_RUNNERS[case],
                "suggested_budget_seconds": (
                    self.scorecard.CASE_SUGGESTED_BUDGET_SECONDS[case]
                ),
                "calculated_phase_cap_seconds": (
                    self.scorecard.CASE_DEADLINES_SECONDS[case]
                ),
                "deadline_carryover_seconds": 0,
            },
        )
        artifacts = {
            name: {"bytes": 1, "sha256": "a" * 64}
            for name in (
                "mpla-speed-poc-v1",
                "mpla-poc-oracle",
                "sandbox-runtime-cli",
                "sandbox-catalog-export",
            )
        }
        self.scorecard.write_new_json(
            root / "staged-artifacts.json",
            {"artifacts": artifacts},
        )
        if case == "qualification" and include_warm_receipt:
            self.scorecard.write_new_json(
                root / self.scorecard.WARM_CACHE_RECEIPT_FILE,
                {
                    **self.warm_cache_receipt(run_id),
                    **(warm_receipt_overrides or {}),
                },
            )
        self.scorecard.Campaign("phase-root-test", root, case).seal_manifest()
        return root

    def cold_cache_receipt(self) -> dict:
        return {
            "schema_version": 1,
            "phase": "F0-COLD",
            "phase_liveness_cap_seconds": 30,
            "cold_build_acceptance_target_ms": 5_000,
            "status": "ok",
            "fixture_profile": self.scorecard.PREPARED_FIXTURE_PROFILE,
            "cold_build_service_elapsed_ms": 1_200.0,
            "cold_build_outer_elapsed_ms": 1_400.0,
            "cold_build_orchestration_overhead_ms": 200.0,
            "docker_setup_elapsed_ms": 100.0,
            "artifact_staging_elapsed_ms": 50.0,
            "launcher_elapsed_before_cleanup_ms": 1_550.0,
            "cold_build_under_5s": True,
            "payload_bytes_were_copied": False,
            "service_result": {
                "fixture_profile": self.scorecard.PREPARED_FIXTURE_PROFILE,
                "manifest_path": (
                    "/eos/mpla-fixtures/s4-chain-sparse-v1/"
                    "PREPARED-FIXTURE.json"
                ),
                "chain_depth": 8,
                "logical_bytes": 8 * 1024 * 1024 * 1024,
                "allocation_count": 8,
                "allocated_bytes": 0,
                "payload_bytes_read": 0,
                "payload_bytes_copied": 0,
                "builder_elapsed_ns": 1_200_000_000,
            },
            "staging_root": "/tmp/mpla-fixture-builder-stage",
        }

    def create_cold_cache_root(self, parent: pathlib.Path) -> pathlib.Path:
        root = parent / "cold-cache"
        root.mkdir()
        self.scorecard.write_new_json(
            root / self.scorecard.COLD_CACHE_RECEIPT_FILE,
            self.cold_cache_receipt(),
        )
        return root

    def warm_cache_receipt(self, phase_run_id: str) -> dict:
        return {
            **self.publication_preparation_summary(),
            "schema_version": 1,
            "phase": "P0-WARM",
            "phase_liveness_cap_seconds": 5,
            "attachment_service_target_ns": 50_000_000,
            "fixture_preparation_target_ns": 1_000_000_000,
            "payload_bytes_were_copied": False,
            "attachment_run_id": f"{phase_run_id}-p0-warm",
            "phase_run_id": phase_run_id,
        }

    def publication_preparation_summary(self) -> dict:
        return {
            "result_path": "/workspace/scorecard-publication-preparation.json",
            "fixture_profile": "s4-chain-sparse-v1",
            "fixture_logical_bytes": 8 * 1024 * 1024 * 1024,
            "chain_depth": 8,
            "prepared_depth_one_candidates": 3,
            "attachment_operation": "attach_mpla_prepared_fixture",
            "attachment_service_elapsed_ns": 23_000_000,
            "payload_bytes_copied": 0,
            "cached_allocation_count": 8,
            "attached_branches": [
                "fixture-depth-1",
                "fixture-depth-5",
                "fixture-depth-8",
            ],
            "fixture_preparation_elapsed_ns": 530_000_000,
        }

    def publication_preparation_campaign(
        self,
        evidence_root: pathlib.Path,
        run_id: str,
        summary: dict,
        *,
        case: str = "publication",
    ):
        for gate in self.scorecard.CASE_GATES[case]:
            (evidence_root / "cases" / gate).mkdir(parents=True)
        if not self.scorecard.CASE_GATES[case]:
            (evidence_root / "cases" / case).mkdir(parents=True)
        campaign = self.scorecard.Campaign(run_id, evidence_root, case)
        campaign.candidate_sandbox_id = "eos-candidate"
        campaign.coordinator_sandbox_id = "eos-coordinator"
        campaign.workspace_session_id = "workspace-session"
        calls = []
        campaign.runtime = lambda _sandbox_id, args, action, timeout: calls.append(
            (args, action, timeout)
        ) or {
            "status": "running",
            "start_offset": 0,
            "end_offset": 0,
            "total_lines": 0,
            "output": "",
            "command_session_id": "command-session",
        }
        campaign.wait_command = lambda *args, **kwargs: {
            "status": "ok",
            "exit_code": 0,
            "output": self.scorecard.RESULT_PREFIX + json.dumps(summary),
            "command_session_id": "command-session",
            "command_total_time_seconds": 0.6,
        }
        return campaign, calls

    def test_each_coordinator_uses_its_phase_local_ceiling(self) -> None:
        self.assertEqual(
            self.scorecard.SCORECARD_CASES,
            (
                "qualification",
                "activation",
                "fork",
                "rollback",
                "publication",
                "squash",
                "stream",
                "recovery",
            ),
        )
        self.assertEqual(
            self.scorecard.PHASE_CASES,
            (*self.scorecard.SCORECARD_CASES, "sealing"),
        )
        self.assertEqual(
            self.scorecard.CASE_DEADLINES_SECONDS,
            {
                "qualification": 60,
                "activation": 120,
                "fork": 60,
                "rollback": 60,
                "publication": 70,
                "squash": 60,
                "stream": 40,
                "recovery": 120,
                "sealing": 30,
            },
        )
        self.assertEqual(
            self.scorecard.CASE_SUGGESTED_BUDGET_SECONDS,
            {
                "qualification": 60,
                "activation": 60,
                "fork": 30,
                "rollback": 30,
                "publication": 35,
                "squash": 30,
                "stream": 20,
                "recovery": 60,
                "sealing": 30,
            },
        )
        self.assertEqual(
            self.scorecard.PUBLICATION_PREPARATION_DEADLINE_SECONDS,
            5,
        )
        self.assertEqual(
            self.scorecard.LIFECYCLE_CONTROL_PREPARATION_DEADLINE_SECONDS,
            120,
        )
        self.assertFalse(hasattr(self.scorecard, "campaign_deadline_seconds"))
        source = SCRIPT.read_text()
        self.assertNotIn("campaign_deadline", source)
        self.assertNotIn("480", source)
        self.assertNotIn("600", source)
        self.assertLess(
            source.index("self.run_lifecycle_control_preparation()"),
            source.index("self.phase_started_ns = time.monotonic_ns()"),
        )

    def test_lifecycle_control_preparation_is_separate_and_fail_closed(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            for gate in self.scorecard.CASE_GATES["activation"]:
                (evidence_root / "cases" / gate).mkdir(parents=True)
            campaign = self.scorecard.Campaign(
                "mpla-control-preparation-test",
                evidence_root,
                "activation",
            )
            campaign.candidate_sandbox_id = "eos-candidate"
            campaign.coordinator_sandbox_id = "eos-coordinator"
            campaign.workspace_session_id = "workspace-session"
            calls = []

            def runtime(sandbox_id, args, action, timeout):
                calls.append((sandbox_id, args, action, timeout))
                return {
                    "status": "running",
                    "command_session_id": "control-preparation-session",
                }

            summary = {
                "schema_version": 1,
                "kind": (
                    "mpla_booster_lifecycle_control_preparation_summary_v1"
                ),
                "run_id": campaign.run_id,
                "phase": "activation",
                "candidate_sandbox_id": "eos-candidate",
                "build_commit": self.scorecard.BUILD_COMMIT,
                "state_root": (
                    "/eos/workspace/mpla-poc/"
                    "scorecard-control-preparations/"
                    f"{campaign.run_id}-activation/state"
                ),
                "catalog_binding_id": "a" * 64,
                "fixture_entries": 4_295,
                "fixture_logical_bytes": 912_350_100,
                "source_manifest_sha256": "b" * 64,
                "control_immutable_publication_count": 1,
                "candidate_immutable_publication_count": 1,
                "immutable_publication_count": 2,
                "control_pre_materialized_carrier_count": 0,
                "candidate_oracle_materialization_count": 1,
                "collection_elapsed_ns": 7_000_000_000,
                "closing_publication_elapsed_ns": 51_000_000_000,
                "candidate_preparation_elapsed_ns": 53_000_000_000,
                "preparation_elapsed_ns": 114_000_000_000,
                "receipt_checksum_sha256": "c" * 64,
            }
            campaign.runtime = runtime
            campaign.wait_command = lambda *args, **kwargs: {
                "status": "ok",
                "exit_code": 0,
                "output": self.scorecard.RESULT_PREFIX + json.dumps(summary),
                "command_session_id": "control-preparation-session",
            }

            campaign.run_lifecycle_control_preparation()

            self.assertEqual(len(calls), 1)
            sandbox_id, args, action, setup_timeout = calls[0]
            self.assertEqual(sandbox_id, "eos-coordinator")
            self.assertEqual(action, "start_activation_control_preparation")
            self.assertEqual(
                setup_timeout,
                self.scorecard.SETUP_OPERATION_DEADLINE_SECONDS,
            )
            frozen = args[-1]
            self.assertIn("prepare-lifecycle-control", frozen)
            self.assertIn("--phase activation", frozen)
            self.assertIn("--candidate-sandbox-id eos-candidate", frozen)
            self.assertEqual(
                args[args.index("--timeout-ms") + 1],
                str(
                    self.scorecard.LIFECYCLE_CONTROL_PREPARATION_DEADLINE_SECONDS
                    * 1000
                ),
            )
            receipt = json.loads(
                (evidence_root / "control-preparation.json").read_text()
            )
            self.assertIs(
                receipt["excluded_from_phase_operation_timer"],
                True,
            )
            self.assertEqual(
                receipt["preparation_liveness_cap_seconds"],
                120,
            )
            self.assertEqual(
                receipt["control_pre_materialized_carrier_count"],
                0,
            )
            self.assertEqual(receipt["candidate_oracle_materialization_count"], 1)

            invalid_root = evidence_root / "invalid"
            for gate in self.scorecard.CASE_GATES["activation"]:
                (invalid_root / "cases" / gate).mkdir(parents=True)
            invalid_campaign = self.scorecard.Campaign(
                "mpla-invalid-control-preparation-test",
                invalid_root,
                "activation",
            )
            invalid_campaign.candidate_sandbox_id = "eos-candidate"
            invalid_campaign.coordinator_sandbox_id = "eos-coordinator"
            invalid_campaign.workspace_session_id = "workspace-session"
            invalid_campaign.runtime = runtime
            invalid_summary = {
                **summary,
                "run_id": invalid_campaign.run_id,
                "state_root": (
                    "/eos/workspace/mpla-poc/"
                    "scorecard-control-preparations/"
                    f"{invalid_campaign.run_id}-activation/state"
                ),
                "control_pre_materialized_carrier_count": 1,
            }
            invalid_campaign.wait_command = lambda *args, **kwargs: {
                "status": "ok",
                "exit_code": 0,
                "output": (
                    self.scorecard.RESULT_PREFIX + json.dumps(invalid_summary)
                ),
                "command_session_id": "control-preparation-session-2",
            }
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "control preparation summary is incomplete",
            ):
                invalid_campaign.run_lifecycle_control_preparation()

    def test_phase_declarations_are_local_and_have_no_deadline_carryover(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for case in self.scorecard.PHASE_CASES:
                evidence_root = root / case
                evidence_root.mkdir()
                campaign = self.scorecard.Campaign(
                    f"mpla-{case}-declaration-test",
                    evidence_root,
                    case,
                )

                campaign.write_phase_declaration()

                declaration = json.loads(
                    (evidence_root / "phase-declaration.json").read_text()
                )
                self.assertEqual(declaration["phase"], case)
                self.assertEqual(
                    declaration["runner"],
                    self.scorecard.CASE_RUNNERS[case],
                )
                self.assertEqual(
                    declaration["suggested_budget_seconds"],
                    self.scorecard.CASE_SUGGESTED_BUDGET_SECONDS[case],
                )
                self.assertEqual(
                    declaration["calculated_phase_cap_seconds"],
                    self.scorecard.CASE_DEADLINES_SECONDS[case],
                )
                self.assertEqual(declaration["deadline_carryover_seconds"], 0)
                self.assertNotIn("campaign_cap_seconds", declaration)
                self.assertNotIn("aggregate_cap_seconds", declaration)

    def test_scorecard_tool_freshness_rejects_artifact_older_than_sources(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            artifact = root / "mpla-speed-poc-v1"
            source = root / "mpla_publication_scorecard.rs"
            artifact.write_bytes(b"old executable")
            source.write_text("new scorecard source\n")
            os.utime(artifact, (1, 1))
            os.utime(source, (2, 2))

            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "scorecard artifact is older than its source inputs",
            ):
                self.scorecard.require_fresh_scorecard_tools(
                    (artifact,),
                    (source,),
                )

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

            with mock.patch.object(
                self.scorecard, "COMMON_SCORECARD_TOOL_SOURCE_INPUTS", ()
            ):
                inputs = self.scorecard.cargo_dep_info_source_inputs(artifact)

                self.assertIn(source, inputs)
                self.assertNotIn(unrelated, inputs)
                self.scorecard.require_fresh_scorecard_tools((artifact,), inputs)

    def test_scorecard_artifacts_use_separate_cargo_dep_info_inputs(self) -> None:
        coordinator_inputs = self.scorecard.cargo_dep_info_source_inputs(
            self.scorecard.COORDINATOR
        )
        oracle_inputs = self.scorecard.cargo_dep_info_source_inputs(
            self.scorecard.ORACLE
        )
        coordinator_source = (
            self.scorecard.REPO
            / "crates"
            / "sandbox-runtime"
            / "mpla-poc"
            / "src"
            / "bin"
            / "mpla-speed-poc-v1.rs"
        )
        oracle_source = (
            self.scorecard.REPO
            / "crates"
            / "sandbox-runtime"
            / "mpla-poc"
            / "src"
            / "bin"
            / "mpla-poc-oracle"
            / "main.rs"
        )

        self.assertIn(coordinator_source, coordinator_inputs)
        self.assertIn(oracle_source, oracle_inputs)
        self.assertNotIn(coordinator_source, oracle_inputs)

    def test_qualification_fixture_preparation_uses_an_isolated_run_locator(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            qualification = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root / "qualification",
                "qualification",
            )
            publication = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root / "publication",
                "publication",
            )

            self.assertEqual(
                qualification.fixture_preparation_run_id(),
                "mpla-scorecard-harness-test-p0-warm",
            )
            self.assertEqual(
                publication.fixture_preparation_run_id(),
                "mpla-scorecard-harness-test",
            )

    def test_publication_fixture_preparation_is_separate_and_bounded(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            summary = self.publication_preparation_summary()
            campaign, calls = self.publication_preparation_campaign(
                evidence_root,
                "mpla-scorecard-harness-test",
                summary,
            )

            campaign.run_publication_preparation()

            arguments, action, timeout = calls[0]
            self.assertEqual(action, "start_publication_preparation")
            self.assertEqual(timeout, 120)
            self.assertEqual(
                arguments[arguments.index("--timeout-ms") + 1], "5000"
            )
            self.assertIn("prepare-publication-fixture", arguments[-1])
            self.assertNotIn("--case", arguments[-1])
            self.assertIn(
                "--run-id mpla-scorecard-harness-test ",
                arguments[-1],
            )
            self.assertIn("--fixture-profile s4-chain-sparse-v1", arguments[-1])
            self.assertIn(
                "/workspace/_campaign-tools/mpla-speed-poc-v1",
                arguments[-1],
            )
            self.assertNotIn("/eos/mpla-fixtures/", arguments[-1])
            self.assertEqual(
                json.loads(
                    (
                        evidence_root
                        / "cases"
                        / "BG-PUBLISH-SMALL"
                        / "fixture-preparation-summary.json"
                    ).read_text()
                ),
                summary,
            )
            attachment_receipt = json.loads(
                (evidence_root / "fixture-cache-attachment.json").read_text()
            )
            self.assertEqual(attachment_receipt["phase"], "P0-WARM")
            self.assertEqual(attachment_receipt["phase_liveness_cap_seconds"], 5)
            self.assertEqual(
                attachment_receipt["attachment_operation"],
                "attach_mpla_prepared_fixture",
            )
            self.assertEqual(
                attachment_receipt["fixture_profile"],
                "s4-chain-sparse-v1",
            )
            self.assertEqual(
                attachment_receipt["fixture_logical_bytes"],
                8 * 1024 * 1024 * 1024,
            )
            self.assertEqual(attachment_receipt["chain_depth"], 8)
            self.assertEqual(attachment_receipt["cached_allocation_count"], 8)
            self.assertEqual(
                attachment_receipt["attached_branches"],
                [
                    "fixture-depth-1",
                    "fixture-depth-5",
                    "fixture-depth-8",
                ],
            )
            self.assertEqual(
                attachment_receipt["attachment_service_target_ns"],
                50_000_000,
            )
            self.assertEqual(
                attachment_receipt["fixture_preparation_target_ns"],
                1_000_000_000,
            )
            self.assertIs(attachment_receipt["payload_bytes_were_copied"], False)
            self.assertEqual(
                attachment_receipt["attachment_run_id"],
                "mpla-scorecard-harness-test",
            )
            self.assertEqual(
                attachment_receipt["phase_run_id"],
                "mpla-scorecard-harness-test",
            )

    def test_publication_fixture_preparation_rejects_corrupt_attachment_receipts(
        self,
    ) -> None:
        corruptions = {
            "operation": {"attachment_operation": "build_mpla_prepared_fixture"},
            "profile": {"fixture_profile": "s4-chain-v13"},
            "logical_bytes": {
                "fixture_logical_bytes": 8 * 1024 * 1024 * 1024 - 1
            },
            "chain_depth": {"chain_depth": 7},
            "depth_one_candidates": {"prepared_depth_one_candidates": 2},
            "payload_copy": {"payload_bytes_copied": 1},
            "payload_copy_boolean": {"payload_bytes_copied": False},
            "payload_copy_float": {"payload_bytes_copied": 0.0},
            "allocations": {"cached_allocation_count": 7},
            "branch_order": {
                "attached_branches": [
                    "fixture-depth-8",
                    "fixture-depth-5",
                    "fixture-depth-1",
                ]
            },
            "attachment_elapsed_type": {"attachment_service_elapsed_ns": "23000000"},
            "preparation_elapsed_type": {
                "fixture_preparation_elapsed_ns": "530000000"
            },
        }
        with tempfile.TemporaryDirectory() as temporary:
            parent = pathlib.Path(temporary)
            for name, corruption in corruptions.items():
                evidence_root = parent / name
                summary = {**self.publication_preparation_summary(), **corruption}
                campaign, _calls = self.publication_preparation_campaign(
                    evidence_root,
                    f"mpla-corrupt-attachment-{name}",
                    summary,
                )

                with self.subTest(name=name):
                    with self.assertRaises(self.scorecard.CampaignError):
                        campaign.run_publication_preparation()
                    self.assertFalse(
                        (evidence_root / "fixture-cache-attachment.json").exists()
                    )

            malformed_root = parent / "malformed-json"
            campaign, _calls = self.publication_preparation_campaign(
                malformed_root,
                "mpla-malformed-attachment-json",
                self.publication_preparation_summary(),
            )
            campaign.wait_command = lambda *args, **kwargs: {
                "status": "ok",
                "exit_code": 0,
                "output": self.scorecard.RESULT_PREFIX + "{not-json",
                "command_session_id": "command-session",
                "command_total_time_seconds": 0.6,
            }
            with self.assertRaises(json.JSONDecodeError):
                campaign.run_publication_preparation()
            self.assertFalse(
                (malformed_root / "fixture-cache-attachment.json").exists()
            )

    def test_repeated_warm_preparation_only_dispatches_read_only_attachment(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = pathlib.Path(temporary)
            all_calls = []
            for ordinal in (1, 2):
                run_id = f"mpla-repeat-warm-{ordinal}"
                evidence_root = parent / run_id
                campaign, calls = self.publication_preparation_campaign(
                    evidence_root,
                    run_id,
                    self.publication_preparation_summary(),
                    case="qualification",
                )

                campaign.run_publication_preparation()
                all_calls.extend(calls)

                receipt = json.loads(
                    (evidence_root / "fixture-cache-attachment.json").read_text()
                )
                self.assertEqual(receipt["attachment_run_id"], f"{run_id}-p0-warm")
                self.assertEqual(
                    receipt["attachment_operation"],
                    "attach_mpla_prepared_fixture",
                )
                self.assertEqual(receipt["payload_bytes_copied"], 0)
                self.assertEqual(receipt["cached_allocation_count"], 8)

            self.assertEqual(len(all_calls), 2)
            for arguments, action, timeout in all_calls:
                frozen = arguments[-1]
                self.assertEqual(action, "start_publication_preparation")
                self.assertEqual(
                    timeout,
                    self.scorecard.SETUP_OPERATION_DEADLINE_SECONDS,
                )
                self.assertIn("prepare-publication-fixture", frozen)
                self.assertIn("--fixture-profile s4-chain-sparse-v1", frozen)
                self.assertNotIn("build-publication-fixture-cache", frozen)
                self.assertNotIn("fixture-builder", frozen)
                self.assertNotIn("/eos/mpla-fixtures/", frozen)

            source = SCRIPT.read_text()
            self.assertNotIn("build-publication-fixture-cache", source)
            self.assertIn(
                "readonly: true",
                self.scorecard.M3_SCORECARD_CONFIG.read_text(),
            )

    def test_scorecard_uses_the_closed_sparse_read_only_fixture(self) -> None:
        config = self.scorecard.M3_SCORECARD_CONFIG.read_text()

        self.assertIn(
            "daemon_config_yaml_path: config/mpla-poc-m3-phase-profile-sparse-v1.yml",
            config,
        )
        self.assertIn(
            "gateway_instance_id: mpla-poc-m3-phase-profile-sparse-v1",
            config,
        )
        self.assertIn(
            "name: eos-mpla-prepared-s4-phase-profile-sparse-v1",
            config,
        )
        self.assertIn("MPLA_RUNTIME_GATEWAY_SOCKET: host.docker.internal:7903", config)
        self.assertIn("readonly: true", config)
        self.assertEqual(
            self.scorecard.PREPARED_FIXTURE_PROFILE,
            "s4-chain-sparse-v1",
        )

    def test_coordinator_is_always_dispatched_from_the_staged_toolset(self) -> None:
        source = SCRIPT.read_text()

        self.assertIn(
            'STAGED_COORDINATOR_PATH = "/workspace/_campaign-tools/mpla-speed-poc-v1"',
            source,
        )
        self.assertNotIn("PREPARED_FIXTURE_TOOL_ROOT", source)

    def test_sparse_poll_schedule_covers_validity_ceiling_within_cap(self) -> None:
        delays = [
            self.scorecard.poll_delay_seconds(poll)
            for poll in range(self.scorecard.MAX_POLLS_PER_CASE)
        ]
        self.assertEqual(delays[:5], [1, 2, 4, 11, 11])
        self.assertGreaterEqual(
            sum(delays),
            self.scorecard.CASE_DEADLINES_SECONDS["publication"],
        )
        self.assertEqual(len(delays), 60)

    def test_coordinator_budget_is_the_full_phase_local_cap(self) -> None:
        campaign = self.scorecard.Campaign(
            "mpla-scorecard-harness-test",
            pathlib.Path("/unused"),
            "activation",
        )
        self.assertEqual(
            campaign.coordinator_budget_ms(),
            self.scorecard.CASE_DEADLINES_SECONDS["activation"] * 1_000,
        )

    def test_terminal_failure_preserves_timeout_classification(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            for gate in self.scorecard.CASE_GATES["publication"]:
                (evidence_root / "cases" / gate).mkdir(parents=True)
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "publication",
            )
            campaign.candidate_sandbox_id = "eos-candidate"
            campaign.coordinator_sandbox_id = "eos-coordinator"
            campaign.workspace_session_id = "workspace-session"
            campaign.runtime = lambda *args, **kwargs: {
                "status": "running",
                "start_offset": 0,
                "end_offset": 0,
                "total_lines": 0,
                "output": "",
                "command_session_id": "command-session",
            }
            campaign.wait_command = lambda *args, **kwargs: {
                "status": "timed_out",
                "start_offset": 0,
                "end_offset": 0,
                "total_lines": 0,
                "output": "",
                "command_session_id": "command-session",
                "exit_code": 124,
                "command_total_time_seconds": 70.125,
            }
            campaign.coordinator_trace = lambda *args, **kwargs: {
                "trace_id": "qualification-trace"
            }

            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "status='timed_out'.*exit_code=124.*70.125",
            ):
                campaign.run_scorecard_case("publication")

    def test_failed_coordinator_trace_uses_its_original_request_id(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                pathlib.Path(temporary),
                "qualification",
            )
            captured = []
            campaign.cli = lambda program, args, action, timeout: captured.append(
                (program, args, action, timeout)
            ) or {"trace_id": args[-1]}

            trace = campaign.coordinator_trace("eos-coordinator", "qualification")

            self.assertEqual(trace["trace_id"], "mpla-scorecard-harness-test-qualification-coordinator")
            self.assertEqual(
                captured,
                [
                    (
                        self.scorecard.OBSERVABILITY,
                        [
                            "trace",
                            "--sandbox-id",
                            "eos-coordinator",
                            "--trace-id",
                            "mpla-scorecard-harness-test-qualification-coordinator",
                        ],
                        "qualification_coordinator_trace",
                        60,
                    )
                ],
            )

    def test_publication_failure_targets_the_last_started_candidate_publish(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                pathlib.Path(temporary),
                "publication",
            )
            progress = {
                "file_read": {
                    "content": "\n".join(
                        [
                            json.dumps(
                                {
                                    "stage": "chain_layer_action_started",
                                    "details": {"layer": 2, "action": "publish"},
                                }
                            ),
                            json.dumps(
                                {
                                    "stage": "chain_layer_action_started",
                                    "details": {"layer": 3, "action": "publish"},
                                }
                            ),
                        ]
                    )
                }
            }
            calls = []
            campaign.cli = lambda program, args, action, timeout: calls.append(
                (program, args, action, timeout)
            ) or {"view": action}

            request_id = campaign.publication_candidate_request_id(progress)
            trace = campaign.publication_candidate_trace("eos-candidate", request_id)
            events = campaign.publication_candidate_events("eos-candidate")

            self.assertEqual(
                request_id,
                "mpla-scorecard-harness-test-layer-003-publish",
            )
            self.assertEqual(trace["view"], "publication_candidate_trace")
            self.assertEqual(events["view"], "publication_candidate_events")
            self.assertEqual(
                calls,
                [
                    (
                        self.scorecard.OBSERVABILITY,
                        [
                            "trace",
                            "--sandbox-id",
                            "eos-candidate",
                            "--trace-id",
                            "mpla-scorecard-harness-test-layer-003-publish",
                        ],
                        "publication_candidate_trace",
                        60,
                    ),
                    (
                        self.scorecard.OBSERVABILITY,
                        [
                            "events",
                            "--sandbox-id",
                            "eos-candidate",
                            "--name",
                            "mpla_publication.checkpoint",
                            "--last-n",
                            "128",
                        ],
                        "publication_candidate_events",
                        60,
                    ),
                ],
            )

    def test_fork_failure_preserves_durable_phase_progress(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            for gate in self.scorecard.CASE_GATES["fork"]:
                (evidence_root / "cases" / gate).mkdir(parents=True)
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "fork",
            )
            campaign.candidate_sandbox_id = "eos-candidate"
            campaign.coordinator_sandbox_id = "eos-coordinator"
            campaign.workspace_session_id = "workspace-session"

            def runtime(_sandbox_id, args, _action, _timeout):
                if "file_read" in args:
                    content = '{"stage":"fork_batch_completed"}\n'
                    return {
                        "content": content,
                        "start_line": 1,
                        "num_lines": 1,
                        "total_lines": 1,
                        "bytes_read": len(content.encode()),
                        "total_bytes": len(content.encode()),
                        "next_offset": None,
                        "truncated": False,
                    }
                return {
                    "status": "running",
                    "start_offset": 0,
                    "end_offset": 0,
                    "total_lines": 0,
                    "output": "",
                    "command_session_id": "command-session",
                }

            campaign.runtime = runtime
            campaign.wait_command = lambda *args, **kwargs: {
                "status": "error",
                "start_offset": 0,
                "end_offset": 1,
                "total_lines": 1,
                "output": "",
                "command_session_id": "command-session",
                "exit_code": 2,
                "command_total_time_seconds": 286.282,
            }
            campaign.coordinator_trace = lambda *args, **kwargs: {"trace_id": "trace"}

            with self.assertRaisesRegex(self.scorecard.CampaignError, "exit_code=2"):
                campaign.run_scorecard_case("fork")

            progress = json.loads(
                (
                    evidence_root
                    / "cases"
                    / "BG-FORK"
                    / "coordinator-progress.json"
                ).read_text()
            )
            self.assertEqual(
                progress["path"], "/workspace/scorecard-fork-progress.jsonl"
            )
            self.assertEqual(
                progress["file_read"]["content"],
                '{"stage":"fork_batch_completed"}\n',
            )

    def test_wait_failure_preserves_durable_phase_progress_and_trace(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            for gate in self.scorecard.CASE_GATES["activation"]:
                (evidence_root / "cases" / gate).mkdir(parents=True)
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "activation",
            )
            campaign.phase_started_ns = self.scorecard.time.monotonic_ns()
            campaign.candidate_sandbox_id = "eos-candidate"
            campaign.coordinator_sandbox_id = "eos-coordinator"
            campaign.workspace_session_id = "workspace-session"
            campaign.runtime = lambda *args, **kwargs: {
                "status": "running",
                "start_offset": 0,
                "end_offset": 0,
                "total_lines": 0,
                "output": "",
                "command_session_id": "command-session",
            }
            campaign.wait_command = mock.Mock(
                side_effect=self.scorecard.CampaignError("host deadline")
            )
            campaign.read_progress = lambda *args, **kwargs: {
                "path": "/workspace/scorecard-activation-progress.jsonl",
                "file_read": {"content": '{"stage":"controls_completed"}\n'},
            }
            campaign.coordinator_trace = lambda *args, **kwargs: {
                "trace_id": "activation-trace"
            }

            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "host deadline",
            ):
                campaign.run_scorecard_case("activation")

            progress = json.loads(
                (
                    evidence_root
                    / "cases"
                    / "BG-ACTIVATE-EXACT"
                    / "coordinator-progress.json"
                ).read_text()
            )
            trace = json.loads(
                (
                    evidence_root
                    / "cases"
                    / "BG-ACTIVATE-SAME"
                    / "coordinator-trace.json"
                ).read_text()
            )
            self.assertEqual(
                progress["file_read"]["content"],
                '{"stage":"controls_completed"}\n',
            )
            self.assertEqual(trace["trace_id"], "activation-trace")

    def test_result_retrieval_assembles_validated_line_windows(self) -> None:
        raw = '{\n  "gate": true,\n  "value": 7\n}'
        pages = [
            {
                "content": '{\n  "gate": true,',
                "start_line": 1,
                "num_lines": 2,
                "total_lines": 4,
                "bytes_read": len('{\n  "gate": true,'.encode()),
                "total_bytes": len(raw.encode()),
                "next_offset": 3,
                "truncated": True,
            },
            {
                "content": '  "value": 7\n}',
                "start_line": 3,
                "num_lines": 2,
                "total_lines": 4,
                "bytes_read": len('  "value": 7\n}'.encode()),
                "total_bytes": len(raw.encode()),
                "next_offset": None,
                "truncated": False,
            },
        ]
        calls = []
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            for gate in self.scorecard.CASE_GATES["publication"]:
                (evidence_root / "cases" / gate).mkdir(parents=True)
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "publication",
            )
            campaign.workspace_session_id = "workspace-session"

            def runtime(*args, **kwargs):
                calls.append((args, kwargs))
                return pages[len(calls) - 1]

            campaign.runtime = runtime
            result, digest = campaign.read_result("eos-coordinator", "publication")

            self.assertEqual(result, {"gate": True, "value": 7})
            self.assertEqual(digest, self.scorecard.sha256_bytes(raw.encode()))
            self.assertEqual(
                [
                    call[0][1][call[0][1].index("--offset") + 1]
                    for call in calls
                ],
                ["1", "3"],
            )
            self.assertNotEqual(
                calls[0][0][1][1],
                calls[1][0][1][1],
            )

    def test_partial_phase_set_cannot_claim_p8_correctness(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "sealing",
            )

            self.assertIsNone(campaign.result_correctness({}, True))

    def test_first_phase_construction_rejects_a_stale_or_partial_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary) / "phase-root"
            evidence_root.mkdir()
            (evidence_root / "partial-attempt.json").write_text("{}\n")
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "qualification",
            )

            with mock.patch.object(self.scorecard, "command") as command:
                with self.assertRaisesRegex(
                    self.scorecard.CampaignError,
                    "refusing pre-existing evidence root",
                ):
                    campaign.prepare_evidence()

            command.assert_not_called()
            self.assertEqual(
                (evidence_root / "partial-attempt.json").read_text(),
                "{}\n",
            )

    def test_p8_accepts_only_complete_cold_warm_and_sealed_p0_p7_inputs(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            cold_cache_root = self.create_cold_cache_root(root)
            phase_roots = {
                case: self.create_sealed_phase_root(root, case)
                for case in self.scorecard.SCORECARD_CASES
            }
            p8_root = root / "sealing"
            p8_root.mkdir()
            self.scorecard.write_new_json(
                p8_root / "source.json",
                {"commit": self.scorecard.BUILD_COMMIT},
            )
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                p8_root,
                "sealing",
                phase_roots,
                cold_cache_root,
            )

            results, verified = campaign.load_phase_results()

            self.assertEqual(set(results), set(self.scorecard.SCORECARD_CASES))
            self.assertEqual(set(verified), set(self.scorecard.SCORECARD_CASES))
            inputs = json.loads((p8_root / "phase-inputs.json").read_text())
            self.assertEqual(
                set(inputs),
                {*self.scorecard.SCORECARD_CASES, "F0-COLD", "P0-WARM"},
            )
            cold_receipt_path = (
                cold_cache_root / self.scorecard.COLD_CACHE_RECEIPT_FILE
            )
            warm_receipt_path = (
                phase_roots["qualification"]
                / self.scorecard.WARM_CACHE_RECEIPT_FILE
            )
            self.assertEqual(
                inputs["F0-COLD"]["receipt_sha256"],
                self.scorecard.sha256_file(cold_receipt_path),
            )
            self.assertEqual(
                inputs["P0-WARM"]["receipt_sha256"],
                self.scorecard.sha256_file(warm_receipt_path),
            )
            self.assertEqual(
                inputs["P0-WARM"]["manifest_sha256"],
                inputs["qualification"]["manifest_sha256"],
            )
            for case, phase_root in phase_roots.items():
                self.assertEqual(inputs[case]["root"], str(phase_root.resolve()))

            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "one --cold-cache-root",
            ):
                self.scorecard.Campaign(
                    "mpla-scorecard-missing-cold-root-test",
                    root / "missing-cold-root-sealing",
                    "sealing",
                    phase_roots,
                ).load_phase_results()

            missing = dict(phase_roots)
            missing.pop("recovery")
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "exactly one root for every P0-P7 phase",
            ):
                self.scorecard.Campaign(
                    "mpla-scorecard-missing-root-test",
                    root / "missing-root-sealing",
                    "sealing",
                    missing,
                    cold_cache_root,
                ).load_phase_results()

            duplicate = dict(phase_roots)
            duplicate["recovery"] = duplicate["stream"]
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "phase roots must be distinct",
            ):
                self.scorecard.Campaign(
                    "mpla-scorecard-duplicate-root-test",
                    root / "duplicate-root-sealing",
                    "sealing",
                    duplicate,
                    cold_cache_root,
                ).load_phase_results()

    def test_p8_cli_requires_setup_roots_only_for_sealing(self) -> None:
        phase_roots = {
            case: pathlib.Path(f"/evidence/{case}")
            for case in self.scorecard.SCORECARD_CASES
        }
        cold_cache_root = pathlib.Path("/evidence/cold-cache")

        self.scorecard.validate_cli_phase_inputs(
            "sealing",
            phase_roots,
            cold_cache_root,
        )
        with self.assertRaisesRegex(
            self.scorecard.CampaignError,
            "requires one --cold-cache-root",
        ):
            self.scorecard.validate_cli_phase_inputs(
                "sealing",
                phase_roots,
                None,
            )
        with self.assertRaisesRegex(
            self.scorecard.CampaignError,
            "--cold-cache-root is only valid",
        ):
            self.scorecard.validate_cli_phase_inputs(
                "qualification",
                {},
                cold_cache_root,
            )
        with mock.patch(
            "sys.argv",
            [
                "run-mpla-booster-scorecard",
                "--run-id",
                "sealing-test",
                "--evidence-root",
                "/evidence/p8",
                "--case",
                "sealing",
                "--cold-cache-root",
                str(cold_cache_root),
            ],
        ):
            args = self.scorecard.parse_args()
        self.assertEqual(args.cold_cache_root, cold_cache_root)

    def test_p8_strictly_rejects_corrupt_cold_and_warm_receipts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            cold_cache_root = root / "cold-cache"
            cold_cache_root.mkdir()
            corrupt_cold = {
                **self.cold_cache_receipt(),
                "unexpected_unsealed_claim": True,
            }
            self.scorecard.write_new_json(
                cold_cache_root / self.scorecard.COLD_CACHE_RECEIPT_FILE,
                corrupt_cold,
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "unexpected schema",
            ):
                self.scorecard.Campaign.verify_cold_cache_root(cold_cache_root)

            qualification = self.create_sealed_phase_root(
                root,
                "qualification",
                warm_receipt_overrides={"payload_bytes_were_copied": True},
            )
            verified = self.scorecard.Campaign.verify_phase_root(
                "qualification",
                qualification,
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "exact read-only attachment",
            ):
                self.scorecard.Campaign.verify_warm_cache_receipt(verified)

            missing_warm = self.create_sealed_phase_root(
                root,
                "activation",
            )
            missing_warm_verified = {
                **verified,
                "root": str(missing_warm.resolve()),
            }
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "absent from the qualification root",
            ):
                self.scorecard.Campaign.verify_warm_cache_receipt(
                    missing_warm_verified
                )

    def test_p8_rejects_partial_and_corrupt_phase_roots(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            partial = root / "partial"
            partial.mkdir()
            self.scorecard.write_new_json(
                partial / "source.json",
                {"commit": self.scorecard.BUILD_COMMIT},
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "phase root is not sealed",
            ):
                self.scorecard.Campaign.verify_phase_root("fork", partial)

            corrupt = self.create_sealed_phase_root(root, "rollback")
            raw_result = (
                corrupt / "cases" / "BG-ROLLBACK" / "raw-result.json"
            )
            raw_result.write_text('{"corrupt":true}\n')
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "manifest verification failed for cases/BG-ROLLBACK/raw-result.json",
            ):
                self.scorecard.Campaign.verify_phase_root("rollback", corrupt)

    def test_p8_rejects_a_sealed_but_ineligible_phase_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            failed = self.create_sealed_phase_root(
                root,
                "stream",
                receipt_status="FAIL",
            )

            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "phase eligibility receipt is invalid",
            ):
                self.scorecard.Campaign.verify_phase_root("stream", failed)

    def test_transport_failed_cleanup_still_seals_partial_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary) / "evidence"
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "qualification",
            )

            def unavailable(*_args, **_kwargs):
                raise self.scorecard.CampaignError("inventory transport unavailable")

            campaign.manager = unavailable
            campaign.observability = unavailable

            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "qualification failed after sealing phase evidence",
            ):
                campaign.run()

            cleanup = json.loads((evidence_root / "cleanup.json").read_text())
            self.assertIsNone(cleanup["final_manager"])
            self.assertIsNone(cleanup["final_observability"])
            self.assertEqual(
                json.loads((evidence_root / "scorecard.json").read_text())["status"],
                "FAIL",
            )

    def test_execution_cleanup_resets_case_scoped_identifiers(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "publication",
            )
            campaign.sandbox_ids = ["eos-candidate", "eos-coordinator"]
            campaign.candidate_sandbox_id = "eos-candidate"
            campaign.coordinator_sandbox_id = "eos-coordinator"
            campaign.workspace_session_id = "workspace-session"
            campaign.runtime = lambda *args, **kwargs: {"status": "destroyed"}
            campaign.manager = lambda *args, **kwargs: {"status": "destroyed"}

            campaign.cleanup_execution("publication")

            self.assertEqual(campaign.sandbox_ids, [])
            self.assertIsNone(campaign.candidate_sandbox_id)
            self.assertIsNone(campaign.coordinator_sandbox_id)
            self.assertIsNone(campaign.workspace_session_id)
            self.assertEqual(
                campaign.cleanup["case_cleanup"],
                [
                    {
                        "case": "publication",
                        "workspace_session_id": "workspace-session",
                        "sandbox_ids": ["eos-candidate", "eos-coordinator"],
                    }
                ],
            )

    def test_host_only_sealing_treats_absent_staging_as_removed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                pathlib.Path(temporary),
                "sealing",
            )
            campaign.manager = lambda *args, **kwargs: {"sandboxes": []}
            campaign.observability = lambda *args, **kwargs: {"sandboxes": []}

            campaign.cleanup_all()

            self.assertIsNone(campaign.stage_root)
            self.assertIs(campaign.cleanup["staging_removed"], True)
            self.assertEqual(campaign.cleanup["final_manager"]["sandboxes"], [])
            self.assertEqual(
                campaign.cleanup["final_observability"]["sandboxes"],
                [],
            )

    def test_case_records_follow_formal_gate_evidence_layout(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            for gate in self.scorecard.FORMAL_GATES:
                (evidence_root / "cases" / gate).mkdir(parents=True)
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "activation",
            )

            campaign.write_case_record("activation", "receipt.json", {"ok": True})

            for gate in self.scorecard.CASE_GATES["activation"]:
                self.assertEqual(
                    (evidence_root / "cases" / gate / "receipt.json").read_text(),
                    '{\n  "ok": true\n}\n',
                )

    def test_p8_fails_closed_without_security_proof(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                pathlib.Path(temporary),
                "sealing",
            )
            results = {
                "qualification": {
                    "exact_content_mode_ownership": True,
                    "initial_helper_receipt_valid": True,
                    "rollback_correctness": True,
                    "fork_isolation": True,
                    "ordinary_workload_denied_privilege": True,
                    "wrong_namespace_rejected": True,
                    "wrong_profile_rejected": False,
                },
                "activation": {
                    "initial_oracle": {"exact_match": True},
                    "candidate_checks": {
                        "selected_refs_stable": True,
                        "projections_exact_zero_build": True,
                        "allocations_fresh": True,
                        "lower_allocations_stable": True,
                    },
                },
                "fork": {
                    "initial_oracle": {"exact_match": True},
                    "candidate_checks": {
                        "selected_refs_stable": True,
                        "projections_exact_zero_build": True,
                        "allocations_fresh": True,
                        "lower_allocations_stable": True,
                    },
                },
                "rollback": {
                    "initial_oracle": {"exact_match": True},
                    "candidate_checks": {
                        "selected_refs_stable": True,
                        "projections_exact_zero_build": True,
                        "allocations_fresh": True,
                        "lower_allocations_stable": True,
                    },
                },
                "squash": {
                    "initial_oracle": {"exact_match": True},
                },
                "publication": {
                    "all_oracle_exact": True,
                    "all_zero_immutable_payload_reads": True,
                    "all_no_second_payload_allocation": True,
                    "final_chain_below_ten_gib": True,
                },
                "stream": {
                    "oracle_exact_match": True,
                    "zero_immutable_payload_reads": True,
                    "no_second_payload_allocation": True,
                },
                "recovery": {
                    "fresh_sweep": {
                        "passed": True,
                        "fixture_logical_bytes": 128 * 1024 * 1024,
                        "summary": {"complete_for_requested_mode": True},
                    },
                },
            }

            report = campaign.correctness_report(results, True)

            self.assertEqual(
                report["checks"]["hv07_crash_sweep"]["status"], "PASS"
            )
            self.assertEqual(
                report["checks"]["storage_helper_capability_boundary"]["status"],
                "FAIL",
            )
            self.assertFalse(campaign.result_correctness(results, True))

    def test_security_profile_accepts_server_selected_qualification_helper(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                pathlib.Path(temporary),
                "sealing",
            )
            report = campaign.security_profile_report(
                {
                    "qualification": {
                        "initial_mount": {
                            "response": {
                                "profile_id": "mpla-storage-admin-overlayfs-dac-override-qualification-v1"
                            }
                        },
                        "initial_helper_receipt_valid": True,
                        "restart_storage_cleanup": [
                            {"response": {"action": "quiesce"}},
                            {"response": {"action": "strict_unmount"}},
                            {"response": {"action": "cleanup"}},
                        ],
                        "ordinary_workload_denied_privilege": True,
                        "wrong_namespace_rejected": True,
                        "wrong_profile_rejected": True,
                    }
                }
            )

            self.assertTrue(report["exact_profile"])
            self.assertEqual(report["status"], "PASS")

    def test_resources_report_rejects_missing_case_receipts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                pathlib.Path(temporary),
                "sealing",
            )
            sample = {
                "process_rss_bytes": 8 * 1024 * 1024,
                "process_io_rchar_bytes": 1,
                "process_io_wchar_bytes": 1,
                "process_io_read_bytes": 1,
                "process_io_write_bytes": 1,
                "cgroup_memory_current_bytes": 16 * 1024 * 1024,
                "cgroup_memory_peak_bytes": 16 * 1024 * 1024,
                "open_fds": 10,
                "run_tree_logical_bytes": 0,
                "run_tree_allocated_bytes": 0,
                "run_tree_inodes": 1,
            }
            resources = {
                "memory_high": 96 * 1024 * 1024,
                "memory_max": 128 * 1024 * 1024,
                "oom_before": 0,
                "oom_after": 0,
                "oom_kill_before": 0,
                "oom_kill_after": 0,
                "baseline": sample,
                "maxima": sample,
                "final_sample": sample,
            }
            results = {
                case: {
                    "schema_version": 1,
                    "kind": self.scorecard.CASE_RESULT_KINDS[case],
                    "resource_bounds": True,
                    "resources": resources,
                }
                for case in ("qualification", "publication", "stream")
            }

            report = campaign.resources_report(results)

            self.assertEqual(
                report["memory_inode_fd_oom_samples"]["publication"]["status"],
                "PASS",
            )
            self.assertEqual(
                report["memory_inode_fd_oom_samples"]["activation"]["status"],
                "FAIL",
            )
            self.assertEqual(report["status"], "FAIL")

            for case in (
                "activation",
                "fork",
                "rollback",
                "squash",
                "recovery",
            ):
                results[case] = {
                    "schema_version": 1,
                    "kind": self.scorecard.CASE_RESULT_KINDS[case],
                    "resource_bounds": True,
                    "resources": resources,
                }

            self.assertEqual(campaign.resources_report(results)["status"], "PASS")

    def test_manifest_verification_is_external_to_its_covered_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                evidence_root,
                "qualification",
            )
            campaign.result_gates = lambda _results: {
                "BG-ACTIVATE-EXACT": False,
                "BG-ACTIVATE-SAME": False,
                "BG-FORK": False,
                "BG-ROLLBACK": False,
                "BG-PUBLISH-SMALL": False,
                "AG-SQUASH": False,
                "AG-STREAM": False,
            }
            campaign.correctness_report = lambda _results, _cleanup: {"checks": {}}
            campaign.result_correctness = lambda _results, _cleanup: False
            campaign.security_profile_report = lambda _results: {"status": "FAIL"}
            campaign.resources_report = lambda _results: {"status": "FAIL"}

            result = campaign.finalize_evidence({}, True)

            scorecard = json.loads((evidence_root / "scorecard.json").read_text())
            verification = json.loads(
                (evidence_root / "manifest-verification.json").read_text()
            )
            manifest = (evidence_root / "manifest.sha256").read_text()
            self.assertEqual(
                scorecard["manifest_verification_record"],
                "manifest-verification.json",
            )
            self.assertEqual(verification["manifest_sha256"], result["manifest_sha256"])
            self.assertTrue(verification["verified"])
            self.assertIn("scorecard.json", manifest)
            self.assertNotIn("manifest-verification.json", manifest)
            for line in manifest.splitlines():
                expected, relative = line.split("  ", 1)
                self.assertEqual(
                    self.scorecard.sha256_file(evidence_root / relative), expected
                )


if __name__ == "__main__":
    unittest.main()
