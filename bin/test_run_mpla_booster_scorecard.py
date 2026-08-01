from __future__ import annotations

import copy
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

    def hv07_result(self) -> dict:
        points = []
        for ordinal, fault_point in enumerate(self.scorecard.HV07_FAULT_POINTS):
            operation_id = f"hv07-operation-{ordinal:02}"
            selected_visibility = self.scorecard.HV07_EXPECTED_VISIBILITY[
                fault_point
            ]
            replay = {
                "fault_point": fault_point,
                "operation_id": operation_id,
                "retry_operation_id": operation_id,
                "selected_visibility": selected_visibility,
                **{
                    field: True
                    for field in (
                        "recovery_invoked",
                        "recovery_completed",
                        "terminal_invariant_verified",
                        "exact_owner_verified",
                        "exact_locator_verified",
                        "exact_ref_verified",
                        "stationary_payload_verified",
                        "failed_attempt_bundle_durable",
                        "cancelled_attempt_bundle_durable",
                        "idempotent_retry_verified",
                    )
                },
            }
            observation = {
                "fault_point": fault_point,
                "operation_id": operation_id,
                "retry_operation_id": operation_id,
                "execution_mode": "process_sigkill",
                "selected_visibility": selected_visibility,
                "idempotent_retry_same_result": True,
                "post_sealing_session_resumed": False,
                "failed_span_retained": True,
                "cancelled_span_retained": True,
                "unclassified_debt_bytes": 0,
                "physical_kill_witness": {
                    "fault_point": fault_point,
                    "operation_id": operation_id,
                    "signal": 9,
                    "durable_marker_observed": True,
                    "marker_parent_synced": True,
                    "terminated_by_expected_signal": True,
                },
                "real_operation_witness": {
                    "fault_point": fault_point,
                    "operation_id": operation_id,
                    "stationary_payload_path_before": "/payload",
                    "stationary_payload_path_after": "/payload",
                    "payload_bytes_moved": 0,
                    "payload_bytes_copied": 0,
                },
                "recovery_replay_witness": replay,
            }
            points.append(
                {
                    "fault_point": fault_point,
                    "passed": True,
                    "assertions": [
                        {"name": name, "passed": True}
                        for name in self.scorecard.HV07_REQUIRED_POINT_ASSERTIONS
                    ],
                    "details": {
                        "record": {
                            "schema_version": 1,
                            "format": "mpla-poc-crash-sweep-v1",
                            "passed": True,
                            "failures": [],
                            "record_sha256": f"{ordinal:064x}",
                            "observation": observation,
                        }
                    },
                }
            )
        point_count = len(points)
        return {
            "schema_version": 1,
            "kind": "mpla_hv07_scorecard_result_v1",
            "fixture_logical_bytes": 128 * 1024 * 1024,
            "acceptance_budget_seconds": 60,
            "outer_watchdog_seconds": 120,
            "campaign_elapsed_ns": 59_000_000_000,
            "outer_watchdog_pass": True,
            "test_exit_code": 0,
            "fresh_sweep": {
                "schema_version": 1,
                "case_id": "HV-07",
                "fixture_logical_bytes": 128 * 1024 * 1024,
                "required_fault_points": point_count,
                "canonical_semantic_builds": 1,
                "semantic_receipt_reuses": point_count - 1,
                "elapsed_ns": 58_000_000_000,
                "hard_stop_ns": 60_000_000_000,
                "summary": {
                    "required_fault_points": point_count,
                    "recorded_attempts": point_count,
                    "passing_fault_points": point_count,
                    "physical_passing_fault_points": point_count,
                    "failed_attempts": 0,
                    "missing_fault_points": [],
                    "physical_missing_fault_points": [],
                    "complete_for_requested_mode": True,
                },
                "points": points,
                "failures": [],
                "passed": True,
            },
        }

    def create_sealed_phase_root(
        self,
        parent: pathlib.Path,
        case: str,
        *,
        receipt_status: str = "PASS",
        include_warm_receipt: bool = True,
        warm_receipt_overrides: dict | None = None,
        sealed_artifact_blob: bool = False,
        run_id_override: str | None = None,
        created_utc_override: str | None = None,
        receipt_overrides: dict | None = None,
        environment_overrides: dict | None = None,
        declaration_overrides: dict | None = None,
        decision_overrides: dict | None = None,
        result_overrides: dict | None = None,
    ) -> pathlib.Path:
        root = parent / case
        root.mkdir()
        run_id = run_id_override or f"phase-root-test-{case}"
        required = receipt_status == "PASS"
        result = {
            "schema_version": 1,
            "kind": self.scorecard.CASE_RESULT_KINDS[case],
            "run_id": run_id,
            "build_commit": self.scorecard.BUILD_COMMIT,
        }
        if case in {"fork", "rollback", "squash", "recovery"}:
            result.update(
                {
                    "phase": case,
                    "runner": self.scorecard.CASE_RUNNERS[case],
                }
            )
        if case == "activation":
            result.update(
                {
                    "activate_exact_gate": {
                        "required": required,
                        "preferred": required,
                    },
                    "activate_same_gate": {
                        "required": required,
                        "preferred": required,
                    },
                }
            )
        elif case == "fork":
            result["fork_gate"] = {
                "required": required,
                "preferred": required,
            }
        elif case == "rollback":
            result["rollback_gate"] = {
                "required": required,
                "preferred": required,
            }
        elif case == "publication":
            result["gate"] = {
                "required": required,
                "preferred": required,
            }
        elif case == "squash":
            result["squash_gate"] = {"required": required}
        elif case == "stream":
            result["required"] = required
            result["preferred"] = required
        result.update(result_overrides or {})
        decision_gates = self.scorecard.phase_result_gates(case, result)
        raw_paths = [
            root / "cases" / gate / "raw-result.json"
            for gate in self.scorecard.CASE_GATES[case]
        ] or [root / "cases" / case / "raw-result.json"]
        for path in raw_paths:
            path.parent.mkdir(parents=True, exist_ok=True)
            self.scorecard.write_new_json(path, result)
        self.scorecard.write_new_json(
            root / "source.json",
            {
                "commit": self.scorecard.BUILD_COMMIT,
                "tree": "1" * 40,
                "tracked_diff_sha256": self.scorecard.sha256_bytes(b""),
                "tracked_diff_bytes": 0,
                "porcelain_sha256": self.scorecard.sha256_bytes(b""),
                "porcelain": "",
                "worktree_files": {},
            },
        )
        created_utc = created_utc_override or (
            "2026-07-30T00:"
            f"{self.scorecard.SCORECARD_CASES.index(case):02}:00+00:00"
        )
        self.scorecard.write_new_json(
            root / "environment.json",
            {
                "run_id": run_id,
                "case": case,
                "image": self.scorecard.IMAGE,
                "platform": "linux/arm64",
                "gateway_socket": self.scorecard.GATEWAY_SOCKET,
                "gateway_config": str(self.scorecard.M3_SCORECARD_CONFIG),
                "r0": str(self.scorecard.R0),
                "build_commit": self.scorecard.BUILD_COMMIT,
                "created_utc": created_utc,
                **(environment_overrides or {}),
            },
        )
        self.scorecard.write_new_json(
            root / "decision.json",
            {
                "schema_version": 1,
                "kind": "mpla_booster_phase_decision_v1",
                "run_id": run_id,
                "phase": case,
                "status": receipt_status,
                "gates": decision_gates,
                **(decision_overrides or {}),
            },
        )
        suggested_budget = self.scorecard.CASE_SUGGESTED_BUDGET_SECONDS[case]
        phase_cap = self.scorecard.CASE_DEADLINES_SECONDS[case]
        selected_multiplier_milli = phase_cap * 1_000 // suggested_budget
        self.scorecard.write_new_json(
            root / "phase-receipt.json",
            {
                "schema_version": 1,
                "kind": "mpla_booster_phase_receipt_v1",
                "run_id": run_id,
                "phase": case,
                "runner": self.scorecard.CASE_RUNNERS[case],
                "suggested_budget_seconds": suggested_budget,
                "selected_multiplier_milli": selected_multiplier_milli,
                "calculated_phase_cap_seconds": phase_cap,
                "deadline_carryover_seconds": 0,
                "clock": "CLOCK_MONOTONIC",
                "elapsed_wall_ns": 1_000_000,
                "total_harness_wall_ns": 2_000_000,
                "timer_scope": (
                    "focused coordinator operation and matched controls only"
                ),
                "cap_pass": True,
                "cleanup_pass": True,
                "status": receipt_status,
                "failures": (
                    []
                    if receipt_status == "PASS"
                    else ["phase-local required gate did not pass"]
                ),
                **(receipt_overrides or {}),
            },
        )
        self.scorecard.write_new_json(
            root / "phase-declaration.json",
            {
                "schema_version": 1,
                "kind": "mpla_booster_phase_declaration_v1",
                "run_id": run_id,
                "phase": case,
                "runner": self.scorecard.CASE_RUNNERS[case],
                "suggested_budget_seconds": suggested_budget,
                "selected_multiplier_milli": selected_multiplier_milli,
                "calculated_phase_cap_seconds": phase_cap,
                "deadline_carryover_seconds": 0,
                "bounded_work": f"one fresh {case} scorecard process and cleanup",
                "declared_utc": created_utc,
                **(declaration_overrides or {}),
            },
        )
        artifacts = {
            name: {"bytes": 1, "sha256": "a" * 64}
            for name in (
                "mpla-speed-poc-v1",
                "mpla-poc-oracle",
                "sandbox-runtime-cli",
                "sandbox-catalog-export",
                "gateway.token",
                "product-catalog.json",
            )
        }
        artifacts["mpla-speed-poc-v1"] = {
            "bytes": self.scorecard.COORDINATOR.stat().st_size,
            "sha256": self.scorecard.sha256_file(self.scorecard.COORDINATOR),
        }
        if sealed_artifact_blob and case != "recovery":
            blob_path = root / "artifacts" / "mpla-poc-oracle"
            blob_path.parent.mkdir()
            self.scorecard.write_new_bytes(blob_path, b"sealed-oracle")
            artifacts["mpla-poc-oracle"] = {
                "bytes": blob_path.stat().st_size,
                "sha256": self.scorecard.sha256_file(blob_path),
                "sealed_path": "artifacts/mpla-poc-oracle",
            }
        if case == "recovery":
            artifacts["hv07_campaign"] = {
                "bytes": 1,
                "sha256": "b" * 64,
            }
            artifact_root = root / "artifacts"
            artifact_root.mkdir(exist_ok=True)
            for name in artifacts:
                payload = (
                    self.scorecard.COORDINATOR.read_bytes()
                    if name == "mpla-speed-poc-v1"
                    else f"sealed-{name}".encode()
                )
                blob_path = artifact_root / name
                self.scorecard.write_new_bytes(blob_path, payload)
                artifacts[name] = {
                    "bytes": len(payload),
                    "sha256": self.scorecard.sha256_bytes(payload),
                    "sealed_path": f"artifacts/{name}",
                }
        self.scorecard.write_new_json(
            root / "staged-artifacts.json",
            {"artifacts": artifacts},
        )
        if case == "qualification" and include_warm_receipt:
            self.scorecard.write_new_json(
                root
                / "cases"
                / "qualification"
                / "fixture-preparation-summary.json",
                self.publication_preparation_summary(),
            )
            self.scorecard.write_new_json(
                root / self.scorecard.WARM_CACHE_RECEIPT_FILE,
                {
                    **self.warm_cache_receipt(run_id),
                    **(warm_receipt_overrides or {}),
                },
            )
        self.scorecard.Campaign("phase-root-test", root, case).seal_manifest()
        return root

    def phase_root_input(
        self,
        root: pathlib.Path,
    ) -> tuple[pathlib.Path, str]:
        return (
            root,
            self.scorecard.sha256_file(root / "manifest.sha256"),
        )

    def reseal_phase_root(
        self,
        root: pathlib.Path,
        case: str,
    ) -> tuple[pathlib.Path, str]:
        (root / "manifest.sha256").unlink()
        (root / "manifest-verification.json").unlink()
        manifest_sha256 = self.scorecard.Campaign(
            "phase-root-test-reseal",
            root,
            case,
        ).seal_manifest()
        return root, manifest_sha256

    def cold_cache_receipt(self) -> dict:
        def operation_timing(
            outer_elapsed_ns: int,
            operation: str,
        ) -> dict:
            service_elapsed_ns = (
                outer_elapsed_ns // 2
                if operation in {"activation", "publication"}
                else None
            )
            phase_elapsed_ns = (
                {
                    "pre_storage": outer_elapsed_ns // 4,
                    "semantic_build": outer_elapsed_ns // 3,
                }
                if operation == "publication"
                else None
            )
            semantic_phase_spans = (
                [{"phase": "semantic-total", "elapsed_ns": outer_elapsed_ns // 5}]
                if operation == "publication"
                else None
            )
            return {
                "outer_elapsed_ns": outer_elapsed_ns,
                "service_elapsed_ns": service_elapsed_ns,
                "phase_elapsed_ns": phase_elapsed_ns,
                "semantic_phase_spans": semantic_phase_spans,
            }

        layer_timings = []
        for layer in range(8):
            timing = {
                "layer": layer,
                "write": operation_timing(10_000_000, "write"),
                "publication": operation_timing(20_000_000, "publication"),
            }
            if layer == 0:
                timing["create"] = operation_timing(5_000_000, "create")
                timing["mount"] = operation_timing(5_000_000, "mount")
            else:
                timing["activation"] = operation_timing(
                    10_000_000,
                    "activation",
                )
            layer_timings.append(timing)
        pre_seal_branches = []
        for name, depth in [
            ("fixture-depth-1", 1),
            ("fixture-depth-5", 5),
            ("fixture-depth-8", 8),
        ]:
            pre_seal_branches.append(
                {
                    "branch": name,
                    "chain_depth": depth,
                    "semantic_roots": {"root_id": f"root-{depth}"},
                    "semantic_attribution": {"depth": depth},
                    "root_manifest": {"depth": depth},
                    "projection_roots": {"root_id": f"root-{depth}"},
                    "projection_lower_allocation_ids_newest_first": [
                        f"allocation-{index}" for index in range(depth)
                    ],
                    "projection_kernel_lower_count": depth,
                    "locator_allocation_id": f"allocation-{depth}",
                    "locator_extent_count": depth,
                    "locator_accounted_bytes": depth * 1024 * 1024 * 1024,
                }
            )
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
                "control_source_logical_bytes": 1025 * 1024 * 1024,
                "allocation_count": 8,
                "allocated_bytes": 0,
                "payload_bytes_read": 0,
                "payload_bytes_copied": 0,
                "recovered": False,
                "removed_fixture_owned_paths": [],
                "builder_elapsed_ns": 1_200_000_000,
                "layer_timings": layer_timings,
                "pre_seal_validation": {
                    "read_only_reopen": True,
                    "exact_branch_count": 3,
                    "branches": pre_seal_branches,
                    "paired_ref": {
                        "format": "mpla-poc-paired-ref-layout-v3",
                        "journal_data_bytes": 64 * 1024 * 1024,
                        "journal_total_bytes": 64 * 1024 * 1024 + 8192,
                        "cursor_generation": 3,
                        "cursor_slot": 0,
                        "logical_end": 4096,
                        "record_count": 10,
                        "last_record_hash": "b" * 64,
                    },
                },
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

    def cold_cache_root_input(
        self,
        root: pathlib.Path,
    ) -> tuple[pathlib.Path, str]:
        return (
            root,
            self.scorecard.sha256_file(
                root / self.scorecard.COLD_CACHE_RECEIPT_FILE
            ),
        )

    def preserved_lineage_inputs(self) -> tuple[dict, dict]:
        def identity(marker: str, byte_count: int = 1) -> dict:
            return {"bytes": byte_count, "sha256": marker * 64}

        def source(marker: str) -> dict:
            return {
                "commit": self.scorecard.BUILD_COMMIT,
                "tree": marker * 40,
                "tracked_diff_sha256": self.scorecard.sha256_bytes(b""),
                "tracked_diff_bytes": 0,
                "porcelain_sha256": self.scorecard.sha256_bytes(b""),
                "porcelain": "",
                "worktree_files": {},
            }

        current_source = source("f")
        verified = {}
        for index, case in enumerate(self.scorecard.SCORECARD_CASES):
            marker = format(index + 1, "x")
            artifacts = {
                name: identity(marker, index + 1)
                for name in (
                    "mpla-speed-poc-v1",
                    "mpla-poc-oracle",
                    "sandbox-runtime-cli",
                    "sandbox-catalog-export",
                    "gateway.token",
                    "product-catalog.json",
                )
            }
            if case == "recovery":
                artifacts["hv07_campaign"] = identity("e")
                sealed_file_bytes = {
                    f"artifacts/{name}": f"p7-{name}".encode()
                    for name in artifacts
                }
                artifacts = {
                    name: {
                        "bytes": len(sealed_file_bytes[f"artifacts/{name}"]),
                        "sha256": self.scorecard.sha256_bytes(
                            sealed_file_bytes[f"artifacts/{name}"]
                        ),
                        "sealed_path": f"artifacts/{name}",
                    }
                    for name in artifacts
                }
            verified[case] = {
                "source": (
                    copy.deepcopy(current_source)
                    if case == "recovery"
                    else source(marker)
                ),
                "manifest_sha256": marker * 64,
                "source_sha256": "1" * 64,
                "staged_artifacts_sha256": "2" * 64,
                "receipt_sha256": "3" * 64,
                "environment_sha256": "4" * 64,
                "staged_artifacts": {"artifacts": artifacts},
                "raw_result_binding": self.scorecard.expected_raw_result_binding(
                    case
                ),
                **(
                    {
                        "_sealed_file_bytes": sealed_file_bytes,
                        "_manifest_digests": {
                            path: self.scorecard.sha256_bytes(payload)
                            for path, payload in sealed_file_bytes.items()
                        },
                    }
                    if case == "recovery"
                    else {}
                ),
            }
        return verified, current_source

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
            "result_sha256": "a" * 64,
            "result_bytes": 4096,
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

    def publication_control_preparation_summary(self, run_id: str) -> dict:
        return {
            "schema_version": 2,
            "kind": (
                "mpla_booster_publication_control_preparation_summary_v2"
            ),
            "run_id": run_id,
            "candidate_sandbox_id": "eos-candidate",
            "control_sandbox_ids": [
                "eos-control-1",
                "eos-control-2",
                "eos-control-3",
            ],
            "build_commit": self.scorecard.BUILD_COMMIT,
            "result_path": (
                "/workspace/scorecard-publication-control-preparation.json"
            ),
            "result_sha256": "c" * 64,
            "result_bytes": 4096,
            "fixture_profile": "s4-chain-sparse-v1",
            "base_count": 3,
            "base_logical_bytes": 1024 * 1024 * 1024,
            "delta_file_count": 10,
            "delta_logical_bytes": 1024 * 1024,
            "base_source_manifest_sha256": (
                self.scorecard.PUBLICATION_CONTROL_BASE_SOURCE_MANIFEST_SHA256
            ),
            "delta_source_manifest_sha256": (
                self.scorecard.PUBLICATION_CONTROL_DELTA_SOURCE_MANIFEST_SHA256
            ),
            "bases": [
                {
                    "pair": pair,
                    "control_sandbox_id": f"eos-control-{pair}",
                    "workspace_session_id": f"workspace-control-{pair}",
                    "manifest_version": 2,
                    "root_hash": str(pair) * 64,
                    "layer_count": 2,
                    "source_count": 1,
                    "ignored_count": 0,
                    "destroyed": True,
                    "matched_publication": {
                        "start_boundary": (
                            self.scorecard.MATCHED_PUBLICATION_START_BOUNDARY
                        ),
                        "stop_boundary": (
                            self.scorecard.MATCHED_PUBLICATION_STOP_BOUNDARY
                        ),
                        "admission_gate_included": True,
                        "durable_root_committed": True,
                        "session_closed": True,
                        "span": {
                            "clock": "monotonic_raw",
                            "started_ns": 100,
                            "finished_ns": 200,
                            "elapsed_ns": 100,
                        },
                    },
                    "publish_response_sha256": str(pair + 3) * 64,
                }
                for pair in range(1, 4)
            ],
            "preparation_elapsed_ns": 4_000_000_000,
            "receipt_checksum_sha256": "d" * 64,
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
        campaign.control_sandbox_ids = [
            "eos-control-1",
            "eos-control-2",
            "eos-control-3",
        ]
        campaign.sandbox_ids = [
            "eos-candidate",
            "eos-coordinator",
            *campaign.control_sandbox_ids,
        ]
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

    def bind_publication_controls(self, campaign) -> None:
        campaign.control_sandbox_ids = [
            "eos-control-1",
            "eos-control-2",
            "eos-control-3",
        ]
        campaign.sandbox_ids = [
            "eos-candidate",
            "eos-coordinator",
            *campaign.control_sandbox_ids,
        ]

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
            self.scorecard.PUBLICATION_CONTROL_PREPARATION_DEADLINE_SECONDS,
            120,
        )
        self.assertEqual(
            self.scorecard.LIFECYCLE_CONTROL_PREPARATION_DEADLINE_SECONDS,
            120,
        )
        self.assertFalse(hasattr(self.scorecard, "campaign_deadline_seconds"))
        source = SCRIPT.read_text()
        self.assertNotIn("campaign_deadline", source)
        self.assertNotRegex(source, r"\b(?:480|600)\b")
        self.assertLess(
            source.index("self.run_lifecycle_control_preparation()"),
            source.index("self.phase_started_ns = time.monotonic_ns()"),
        )
        self.assertLess(
            source.index("self.run_publication_preparation()"),
            source.index("self.run_publication_control_preparation()"),
        )
        self.assertLess(
            source.index("self.run_publication_control_preparation()"),
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

    def test_publication_control_preparation_is_separate_and_fail_closed(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            run_id = "mpla-publication-control-preparation-test"
            summary = self.publication_control_preparation_summary(run_id)
            campaign, calls = self.publication_preparation_campaign(
                evidence_root,
                run_id,
                summary,
            )

            campaign.run_publication_control_preparation()

            arguments, action, timeout = calls[0]
            self.assertEqual(
                action,
                "start_publication_control_preparation",
            )
            self.assertEqual(
                timeout,
                self.scorecard.SETUP_OPERATION_DEADLINE_SECONDS,
            )
            self.assertEqual(
                arguments[arguments.index("--timeout-ms") + 1],
                str(
                    self.scorecard.PUBLICATION_CONTROL_PREPARATION_DEADLINE_SECONDS
                    * 1000
                ),
            )
            frozen = arguments[-1]
            self.assertIn("prepare-publication-control", frozen)
            self.assertIn(f"--run-id {run_id} ", frozen)
            self.assertIn("--candidate-sandbox-id eos-candidate", frozen)
            self.assertEqual(
                [
                    frozen.split()[index + 1]
                    for index, argument in enumerate(frozen.split())
                    if argument == "--control-sandbox-id"
                ],
                [
                    "eos-control-1",
                    "eos-control-2",
                    "eos-control-3",
                ],
            )
            self.assertIn(
                f"--build-commit {self.scorecard.BUILD_COMMIT}",
                frozen,
            )
            self.assertIn("--fixture-profile s4-chain-sparse-v1", frozen)
            receipt = json.loads(
                (evidence_root / "control-preparation.json").read_text()
            )
            self.assertEqual(
                receipt["preparation_liveness_cap_seconds"],
                120,
            )
            self.assertIs(
                receipt["excluded_from_phase_operation_timer"],
                True,
            )
            self.assertEqual(
                [base["pair"] for base in receipt["bases"]],
                [1, 2, 3],
            )
            self.assertEqual(
                [
                    base["control_sandbox_id"]
                    for base in receipt["bases"]
                ],
                receipt["control_sandbox_ids"],
            )
            self.assertEqual(
                json.loads(
                    (
                        evidence_root
                        / "cases"
                        / "BG-PUBLISH-SMALL"
                        / "control-preparation-summary.json"
                    ).read_text()
                ),
                summary,
            )

    def test_publication_control_preparation_rejects_wrong_lineage(self) -> None:
        run_id = "mpla-publication-control-corruption-test"
        valid = self.publication_control_preparation_summary(run_id)
        corruptions = {
            "run": {"run_id": f"{run_id}-wrong"},
            "candidate": {"candidate_sandbox_id": "eos-other"},
            "controls": {
                "control_sandbox_ids": [
                    "eos-control-1",
                    "eos-control-3",
                    "eos-control-2",
                ]
            },
            "build": {"build_commit": "0" * 40},
            "base_count": {"base_count": 2},
            "base_bytes": {"base_logical_bytes": 1024 * 1024 * 1024 - 1},
            "delta_files": {"delta_file_count": 9},
            "delta_bytes": {"delta_logical_bytes": 1024 * 1024 - 1},
            "manifest_alias": {
                "delta_source_manifest_sha256": (
                    valid["base_source_manifest_sha256"]
                )
            },
            "schema": {"schema_version": 1},
            "kind": {
                "kind": (
                    "mpla_booster_publication_control_preparation_summary_v1"
                )
            },
        }
        with tempfile.TemporaryDirectory() as temporary:
            parent = pathlib.Path(temporary)
            for name, corruption in corruptions.items():
                evidence_root = parent / name
                summary = {**copy.deepcopy(valid), **corruption}
                campaign, _calls = self.publication_preparation_campaign(
                    evidence_root,
                    run_id,
                    summary,
                )
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "control preparation summary is incomplete",
                    ):
                        campaign.run_publication_control_preparation()
                    self.assertFalse(
                        (evidence_root / "control-preparation.json").exists()
                    )

            base_corruptions = {
                "pair_order": lambda bases: bases.reverse(),
                "shared_control": lambda bases: bases[1].update(
                    control_sandbox_id=bases[0]["control_sandbox_id"]
                ),
                "wrong_control": lambda bases: bases[1].update(
                    control_sandbox_id="eos-other"
                ),
                "manifest_version": lambda bases: bases[0].update(
                    manifest_version=1
                ),
                "root_hash": lambda bases: bases[0].update(
                    root_hash="not-a-digest"
                ),
                "layer_count": lambda bases: bases[0].update(layer_count=1),
                "source_count": lambda bases: bases[0].update(source_count=0),
                "ignored_count": lambda bases: bases[0].update(ignored_count=1),
                "not_destroyed": lambda bases: bases[0].update(destroyed=False),
                "no_durable_root": lambda bases: bases[0][
                    "matched_publication"
                ].update(durable_root_committed=False),
                "wrong_boundary": lambda bases: bases[0][
                    "matched_publication"
                ].update(stop_boundary="wrong"),
                "bad_span": lambda bases: bases[0][
                    "matched_publication"
                ]["span"].update(elapsed_ns=99),
                "wrong_clock": lambda bases: bases[0][
                    "matched_publication"
                ]["span"].update(clock="monotonic"),
                "publish_response_sha256": lambda bases: bases[0].update(
                    publish_response_sha256="not-a-digest"
                ),
                "missing_proof": lambda bases: bases[0].pop("matched_publication"),
            }
            for name, mutate in base_corruptions.items():
                evidence_root = parent / name
                summary = copy.deepcopy(valid)
                mutate(summary["bases"])
                campaign, _calls = self.publication_preparation_campaign(
                    evidence_root,
                    run_id,
                    summary,
                )
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "control preparation summary is incomplete",
                    ):
                        campaign.run_publication_control_preparation()
                    self.assertFalse(
                        (evidence_root / "control-preparation.json").exists()
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
            "name: eos-mpla-prepared-s4-phase-profile-sparse-v1-ref-v3",
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

    def test_publication_prepares_three_tracked_control_sandboxes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign = self.scorecard.Campaign(
                "mpla-publication-control-sandboxes-test",
                pathlib.Path(temporary),
                "publication",
            )
            campaign.stage_root = pathlib.Path(temporary)
            created = []

            def manager(_args, action, _timeout):
                sandbox_id = {
                    "create_candidate_sandbox": "eos-candidate",
                    "create_coordinator_sandbox": "eos-coordinator",
                    "create_publication_control_1_sandbox": "eos-control-1",
                    "create_publication_control_2_sandbox": "eos-control-2",
                    "create_publication_control_3_sandbox": "eos-control-3",
                }[action]
                created.append(sandbox_id)
                return {"id": sandbox_id}

            campaign.manager = manager
            campaign.runtime = lambda sandbox_id, args, action, _timeout: (
                {
                    "workspace_session_id": "workspace-session",
                    "sandbox_id": sandbox_id,
                    "args": args,
                    "action": action,
                }
            )

            campaign.prepare_execution()

            self.assertEqual(
                created,
                [
                    "eos-candidate",
                    "eos-coordinator",
                    "eos-control-1",
                    "eos-control-2",
                    "eos-control-3",
                ],
            )
            self.assertEqual(campaign.sandbox_ids, created)
            self.assertEqual(
                campaign.control_sandbox_ids,
                ["eos-control-1", "eos-control-2", "eos-control-3"],
            )
            self.assertEqual(
                campaign.publication_control_cli(),
                (
                    " --control-sandbox-id eos-control-1"
                    " --control-sandbox-id eos-control-2"
                    " --control-sandbox-id eos-control-3"
                ),
            )

    def test_publication_result_binds_prepared_control_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = pathlib.Path(temporary)
            for binding in ("d" * 64, "e" * 64):
                evidence_root = parent / binding[:1]
                (
                    evidence_root / "cases" / "BG-PUBLISH-SMALL"
                ).mkdir(parents=True)
                self.scorecard.write_new_json(
                    evidence_root / "control-preparation.json",
                    {"receipt_checksum_sha256": "d" * 64},
                )
                campaign = self.scorecard.Campaign(
                    "mpla-publication-binding-test",
                    evidence_root,
                    "publication",
                )
                campaign.candidate_sandbox_id = "eos-candidate"
                campaign.coordinator_sandbox_id = "eos-coordinator"
                self.bind_publication_controls(campaign)
                campaign.workspace_session_id = "workspace-session"
                runtime_calls = []
                campaign.runtime = lambda *args, **kwargs: runtime_calls.append(
                    (args, kwargs)
                ) or {
                    "status": "running",
                    "command_session_id": "command-session",
                }
                campaign.wait_command = lambda *args, **kwargs: {
                    "status": "ok",
                    "exit_code": 0,
                    "output": self.scorecard.RESULT_PREFIX
                    + json.dumps(
                        {
                            "result_path": (
                                "/workspace/"
                                "scorecard-publication-result.json"
                            ),
                            "result_sha256": "a" * 64,
                        }
                    ),
                    "command_session_id": "command-session",
                }
                campaign.read_result = lambda *args, **kwargs: (
                    {
                        "control_preparation": {
                            "checksum_sha256": binding,
                        }
                    },
                    "a" * 64,
                )
                if binding == "d" * 64:
                    result = campaign.run_scorecard_case("publication")
                    frozen = runtime_calls[0][0][1][-1]
                    self.assertEqual(
                        [
                            frozen.split()[index + 1]
                            for index, argument in enumerate(frozen.split())
                            if argument == "--control-sandbox-id"
                        ],
                        campaign.control_sandbox_ids,
                    )
                    self.assertEqual(
                        result["control_preparation"]["checksum_sha256"],
                        "d" * 64,
                    )
                else:
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "does not bind the prepared publication control receipt",
                    ):
                        campaign.run_scorecard_case("publication")

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
            self.bind_publication_controls(campaign)
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

    def test_recovery_result_retrieval_seals_explicit_phase_and_runner(self) -> None:
        producer_result = {
            "schema_version": 1,
            "kind": self.scorecard.CASE_RESULT_KINDS["recovery"],
            "run_id": "p7-explicit-binding",
            "build_commit": self.scorecard.BUILD_COMMIT,
        }
        raw = json.dumps(producer_result)
        page = {
            "content": raw,
            "start_line": 1,
            "num_lines": 1,
            "total_lines": 1,
            "bytes_read": len(raw.encode()),
            "total_bytes": len(raw.encode()),
            "next_offset": None,
            "truncated": False,
        }
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            (evidence_root / "cases" / "HV-07").mkdir(parents=True)
            campaign = self.scorecard.Campaign(
                "p7-explicit-binding",
                evidence_root,
                "recovery",
            )
            campaign.workspace_session_id = "workspace-session"
            campaign.runtime = mock.Mock(return_value=page)

            result, producer_digest = campaign.read_result(
                "eos-coordinator",
                "recovery",
            )

            self.assertEqual(result["phase"], "recovery")
            self.assertEqual(
                result["runner"],
                self.scorecard.CASE_RUNNERS["recovery"],
            )
            self.assertEqual(
                json.loads(
                    (
                        evidence_root
                        / "cases"
                        / "HV-07"
                        / "raw-result.json"
                    ).read_text()
                ),
                result,
            )
            self.assertEqual(
                producer_digest,
                self.scorecard.sha256_bytes(raw.encode()),
            )

    def test_recovery_coordinator_failure_preserves_raw_result_before_trace(
        self,
    ) -> None:
        producer_result = self.hv07_result()
        producer_result["test_exit_code"] = 2
        producer_result["fresh_sweep"]["passed"] = False
        producer_result["fresh_sweep"]["failures"] = ["elapsed budget exceeded"]
        raw = json.dumps(producer_result)
        page = {
            "content": raw,
            "start_line": 1,
            "num_lines": 1,
            "total_lines": 1,
            "bytes_read": len(raw.encode()),
            "total_bytes": len(raw.encode()),
            "next_offset": None,
            "truncated": False,
        }
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            (evidence_root / "cases" / "HV-07").mkdir(parents=True)
            campaign = self.scorecard.Campaign(
                "p7-failed-result-capture",
                evidence_root,
                "recovery",
            )
            campaign.workspace_session_id = "workspace-session"
            campaign.runtime = mock.Mock(return_value=page)

            def coordinator_trace(*_args, **_kwargs):
                case_root = evidence_root / "cases" / "HV-07"
                self.assertTrue((case_root / "raw-result.json").is_file())
                self.assertTrue(
                    (
                        case_root
                        / "coordinator-failure-result-capture.json"
                    ).is_file()
                )
                return {"trace_id": "recovery-failure-trace"}

            campaign.coordinator_trace = coordinator_trace
            campaign.capture_coordinator_failure("recovery", "eos-coordinator")

            case_root = evidence_root / "cases" / "HV-07"
            captured_result = json.loads((case_root / "raw-result.json").read_text())
            receipt = json.loads(
                (
                    case_root / "coordinator-failure-result-capture.json"
                ).read_text()
            )
            self.assertEqual(captured_result["phase"], "recovery")
            self.assertEqual(
                captured_result["runner"],
                self.scorecard.CASE_RUNNERS["recovery"],
            )
            self.assertIs(captured_result["fresh_sweep"]["passed"], False)
            self.assertEqual(
                receipt,
                {
                    "schema_version": 1,
                    "kind": (
                        "mpla_booster_coordinator_failure_result_capture_v1"
                    ),
                    "result_path": "/workspace/hv07-result.json",
                    "producer_sha256": self.scorecard.sha256_bytes(raw.encode()),
                    "result_kind": self.scorecard.CASE_RESULT_KINDS["recovery"],
                    "test_exit_code": 2,
                    "fresh_sweep_passed": False,
                    "retrieved_before_cleanup": True,
                    "scorecard_result_accepted": False,
                    "diagnostic_only": True,
                },
            )
            self.assertEqual(
                json.loads((case_root / "coordinator-trace.json").read_text()),
                {"trace_id": "recovery-failure-trace"},
            )

    def test_recovery_coordinator_failure_seals_result_retrieval_error(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            (evidence_root / "cases" / "HV-07").mkdir(parents=True)
            campaign = self.scorecard.Campaign(
                "p7-failed-result-capture-error",
                evidence_root,
                "recovery",
            )
            campaign.workspace_session_id = "workspace-session"
            campaign.read_result = mock.Mock(
                side_effect=self.scorecard.CampaignError("diagnostic result unavailable")
            )

            def coordinator_trace(*_args, **_kwargs):
                self.assertTrue(
                    (
                        evidence_root
                        / "cases"
                        / "HV-07"
                        / "coordinator-failure-result-capture-error.json"
                    ).is_file()
                )
                return {"trace_id": "recovery-failure-trace"}

            campaign.coordinator_trace = coordinator_trace
            campaign.capture_coordinator_failure("recovery", "eos-coordinator")

            case_root = evidence_root / "cases" / "HV-07"
            receipt = json.loads(
                (
                    case_root
                    / "coordinator-failure-result-capture-error.json"
                ).read_text()
            )
            self.assertEqual(
                receipt,
                {
                    "schema_version": 1,
                    "kind": (
                        "mpla_booster_coordinator_failure_result_capture_error_v1"
                    ),
                    "result_path": "/workspace/hv07-result.json",
                    "error": "diagnostic result unavailable",
                    "retrieved_before_cleanup": False,
                    "scorecard_result_accepted": False,
                    "diagnostic_only": True,
                },
            )
            self.assertFalse((case_root / "raw-result.json").exists())
            self.assertEqual(
                json.loads((case_root / "coordinator-trace.json").read_text()),
                {"trace_id": "recovery-failure-trace"},
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
            phase_paths = {
                case: self.create_sealed_phase_root(root, case)
                for case in self.scorecard.SCORECARD_CASES
            }
            phase_roots = {
                case: self.phase_root_input(phase_root)
                for case, phase_root in phase_paths.items()
            }
            p8_root = root / "sealing"
            p8_root.mkdir()
            self.scorecard.write_new_json(
                p8_root / "source.json",
                {
                    "commit": self.scorecard.BUILD_COMMIT,
                    "tree": "1" * 40,
                    "tracked_diff_sha256": self.scorecard.sha256_bytes(b""),
                    "tracked_diff_bytes": 0,
                    "porcelain_sha256": self.scorecard.sha256_bytes(b""),
                    "porcelain": "",
                    "worktree_files": {},
                },
            )
            self.scorecard.write_new_json(
                p8_root / "environment.json",
                {
                    "run_id": "mpla-scorecard-harness-test",
                    "case": "sealing",
                    "build_commit": self.scorecard.BUILD_COMMIT,
                    "created_utc": "2026-07-30T01:00:00+00:00",
                },
            )
            self.scorecard.write_new_json(
                p8_root / "phase-declaration.json",
                {
                    "run_id": "mpla-scorecard-harness-test",
                    "phase": "sealing",
                    "runner": self.scorecard.CASE_RUNNERS["sealing"],
                    "declared_utc": "2026-07-30T01:01:00+00:00",
                },
            )
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-harness-test",
                p8_root,
                "sealing",
                phase_roots,
                self.cold_cache_root_input(cold_cache_root),
            )

            with mock.patch.object(
                self.scorecard.Campaign,
                "run_scorecard_case",
            ) as run_scorecard_case:
                results, verified = campaign.load_phase_results()
            run_scorecard_case.assert_not_called()

            self.assertEqual(set(results), set(self.scorecard.SCORECARD_CASES))
            self.assertEqual(set(verified), set(self.scorecard.SCORECARD_CASES))
            inputs = json.loads((p8_root / "phase-inputs.json").read_text())
            self.assertEqual(
                set(inputs),
                {
                    *self.scorecard.SCORECARD_CASES,
                    "F0-COLD",
                    "P0-WARM",
                    "P8-ARTIFACT",
                    "cache_generation_equivalence",
                    "lineage",
                },
            )
            self.assertEqual(
                inputs["lineage"]["mode"],
                "sealed-phase-local-history-current-p7",
            )
            self.assertEqual(
                [entry["phase"] for entry in inputs["lineage"]["phase_sequence"]],
                [*self.scorecard.SCORECARD_CASES, "sealing"],
            )
            self.assertEqual(
                inputs["lineage"]["artifact_verification_levels"],
                {"P0-P6": "identity-receipt-only", "P7": "sealed-bytes"},
            )
            self.assertEqual(
                inputs["lineage"]["p8_artifact"]["verification_level"],
                "sealed-bytes",
            )
            self.assertEqual(
                inputs["P8-ARTIFACT"],
                inputs["lineage"]["p8_artifact"],
            )
            self.assertEqual(
                inputs["lineage"]["raw_result_binding_levels"],
                {
                    "P0/P1/P4/P6": (
                        "historical-indirect-kind-and-sealed-receipts"
                    ),
                    "P2/P3/P5/P7": "explicit-raw-phase-runner",
                },
            )
            self.assertEqual(
                inputs["cache_generation_equivalence"]["status"],
                "PASS",
            )
            self.assertFalse(
                inputs["cache_generation_equivalence"][
                    "ref_transport_identity_required"
                ]
            )
            cold_receipt_path = (
                cold_cache_root / self.scorecard.COLD_CACHE_RECEIPT_FILE
            )
            warm_receipt_path = (
                phase_paths["qualification"]
                / self.scorecard.WARM_CACHE_RECEIPT_FILE
            )
            self.assertEqual(
                inputs["F0-COLD"]["receipt_sha256"],
                self.scorecard.sha256_file(cold_receipt_path),
            )
            self.assertEqual(
                inputs["F0-COLD"]["expected_receipt_sha256"],
                inputs["F0-COLD"]["receipt_sha256"],
            )
            self.assertEqual(
                inputs["P0-WARM"]["receipt_sha256"],
                self.scorecard.sha256_file(warm_receipt_path),
            )
            self.assertEqual(
                inputs["P0-WARM"]["manifest_sha256"],
                inputs["qualification"]["manifest_sha256"],
            )
            for case, phase_root in phase_paths.items():
                self.assertEqual(
                    inputs[case]["root"],
                    os.path.abspath(phase_root),
                )
                self.assertEqual(
                    inputs[case]["expected_manifest_sha256"],
                    phase_roots[case][1],
                )
                self.assertEqual(
                    inputs[case]["manifest_sha256"],
                    phase_roots[case][1],
                )

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
                    self.cold_cache_root_input(cold_cache_root),
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
                    self.cold_cache_root_input(cold_cache_root),
                ).load_phase_results()

    def test_p8_accepts_phase_local_history_and_requires_current_p7(self) -> None:
        verified, current_source = self.preserved_lineage_inputs()
        current_identity = verified["recovery"]["staged_artifacts"]["artifacts"][
            "mpla-speed-poc-v1"
        ]
        current_coordinator = (
            current_identity["bytes"],
            current_identity["sha256"],
        )

        for index, case in enumerate(self.scorecard.SCORECARD_CASES[:-1]):
            marker = format(index + 1, "x")
            verified[case]["source"] = copy.deepcopy(current_source)
            verified[case]["source"]["tree"] = marker * 40
            verified[case]["staged_artifacts"]["artifacts"][
                "mpla-poc-oracle"
            ] = {"bytes": index + 1, "sha256": marker * 64}

        lineage = self.scorecard.verify_phase_lineage(
            verified,
            current_source,
            current_coordinator,
        )

        self.assertEqual(
            lineage["mode"],
            "sealed-phase-local-history-current-p7",
        )
        self.assertEqual(
            lineage["preserved_cases"],
            list(self.scorecard.SCORECARD_CASES[:-1]),
        )
        self.assertEqual(lineage["exact_current_cases"], ["recovery"])
        self.assertEqual(
            set(lineage["sealed_phases"]),
            set(self.scorecard.SCORECARD_CASES),
        )

        stale_source = copy.deepcopy(verified)
        stale_source["recovery"]["source"] = copy.deepcopy(
            stale_source["stream"]["source"]
        )
        with self.assertRaisesRegex(
            self.scorecard.CampaignError,
            "P7 source fingerprint",
        ):
            self.scorecard.verify_phase_lineage(
                stale_source,
                current_source,
                current_coordinator,
            )

        stale_artifact = copy.deepcopy(verified)
        stale_payload = b"stale-p7-coordinator"
        stale_digest = self.scorecard.sha256_bytes(stale_payload)
        stale_path = "artifacts/mpla-speed-poc-v1"
        stale_artifact["recovery"]["_sealed_file_bytes"][stale_path] = (
            stale_payload
        )
        stale_artifact["recovery"]["_manifest_digests"][stale_path] = (
            stale_digest
        )
        stale_artifact["recovery"]["staged_artifacts"]["artifacts"][
            "mpla-speed-poc-v1"
        ] = {
            "bytes": len(stale_payload),
            "sha256": stale_digest,
            "sealed_path": stale_path,
        }
        with self.assertRaisesRegex(
            self.scorecard.CampaignError,
            "P7 coordinator identity",
        ):
            self.scorecard.verify_phase_lineage(
                stale_artifact,
                current_source,
                current_coordinator,
            )

        malformed_history = copy.deepcopy(verified)
        malformed_history["activation"]["staged_artifacts"]["artifacts"][
            "mpla-poc-oracle"
        ]["sha256"] = "not-a-digest"
        with self.assertRaisesRegex(
            self.scorecard.CampaignError,
            "activation staged artifact mpla-poc-oracle identity is malformed",
        ):
            self.scorecard.verify_phase_lineage(
                malformed_history,
                current_source,
                current_coordinator,
            )

        malformed_linkage = copy.deepcopy(verified)
        malformed_linkage["fork"]["source_sha256"] = "not-a-digest"
        with self.assertRaisesRegex(
            self.scorecard.CampaignError,
            "fork sealed source.json linkage is malformed",
        ):
            self.scorecard.verify_phase_lineage(
                malformed_linkage,
                current_source,
                current_coordinator,
            )

    def test_p8_exact_current_rejects_unsupported_source_status(self) -> None:
        verified, current_source = self.preserved_lineage_inputs()
        current_identity = verified["recovery"]["staged_artifacts"]["artifacts"][
            "mpla-speed-poc-v1"
        ]
        current_coordinator = (
            current_identity["bytes"],
            current_identity["sha256"],
        )
        malformed_source = copy.deepcopy(current_source)
        malformed_source["porcelain"] = (
            " D bin/run-mpla-booster-scorecard\n"
        )
        malformed_source["porcelain_sha256"] = self.scorecard.sha256_bytes(
            malformed_source["porcelain"].encode()
        )
        for case in self.scorecard.SCORECARD_CASES:
            verified[case]["source"] = copy.deepcopy(malformed_source)

        with self.assertRaisesRegex(
            self.scorecard.CampaignError,
            "source porcelain status is unsupported",
        ):
            self.scorecard.verify_phase_lineage(
                verified,
                malformed_source,
                current_coordinator,
            )

    def test_p8_cli_requires_setup_roots_only_for_sealing(self) -> None:
        phase_roots = {
            case: (pathlib.Path(f"/evidence/{case}"), "a" * 64)
            for case in self.scorecard.SCORECARD_CASES
        }
        cold_cache_root = pathlib.Path("/evidence/cold-cache")
        cold_cache_input = (cold_cache_root, "c" * 64)

        self.scorecard.validate_cli_phase_inputs(
            "sealing",
            phase_roots,
            cold_cache_input,
        )
        parsed = self.scorecard.parse_phase_roots(
            [f"qualification=/evidence/p0@{'b' * 64}"]
        )
        self.assertEqual(
            parsed["qualification"],
            (pathlib.Path("/evidence/p0"), "b" * 64),
        )
        for malformed in (
            "qualification=/evidence/p0",
            f"qualification=/evidence/p0@{'B' * 64}",
            "qualification=/evidence/p0@short",
        ):
            with self.subTest(malformed=malformed):
                with self.assertRaises(
                    self.scorecard.argparse.ArgumentTypeError
                ):
                    self.scorecard.parse_phase_roots([malformed])
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
                cold_cache_input,
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
                f"{cold_cache_root}@{'c' * 64}",
            ],
        ):
            args = self.scorecard.parse_args()
        self.assertEqual(args.cold_cache_root, cold_cache_input)
        for malformed in (
            str(cold_cache_root),
            f"{cold_cache_root}@short",
            f"{cold_cache_root}@{'C' * 64}",
        ):
            with self.subTest(cold_cache_root=malformed):
                with self.assertRaises(SystemExit):
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
                            malformed,
                        ],
                    ):
                        self.scorecard.parse_args()

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
                self.scorecard.Campaign.verify_cold_cache_root(
                    *self.cold_cache_root_input(cold_cache_root)
                )

            qualification = self.create_sealed_phase_root(
                root,
                "qualification",
                warm_receipt_overrides={"payload_bytes_were_copied": True},
            )
            verified = self.scorecard.Campaign.verify_phase_root(
                "qualification",
                qualification,
                self.phase_root_input(qualification)[1],
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
            missing_warm_verified = self.scorecard.Campaign.verify_phase_root(
                "activation",
                *self.phase_root_input(missing_warm),
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "absent from the qualification root",
            ):
                self.scorecard.Campaign.verify_warm_cache_receipt(
                    missing_warm_verified
                )

    def test_p8_accepts_only_exact_cold_recovery_and_root_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            cold_cache_root = root / "cold-cache"
            cold_cache_root.mkdir()
            recovered = self.cold_cache_receipt()
            recovered["service_result"]["recovered"] = True
            recovered["service_result"]["removed_fixture_owned_paths"] = [
                (
                    "/eos/mpla-fixtures/s4-chain-sparse-v1/"
                    "layer-stack/mpla-poc"
                )
            ]
            self.scorecard.write_new_json(
                cold_cache_root / self.scorecard.COLD_CACHE_RECEIPT_FILE,
                recovered,
            )

            cold_cache_input = self.cold_cache_root_input(cold_cache_root)
            verified = self.scorecard.Campaign.verify_cold_cache_root(
                *cold_cache_input
            )

            self.assertTrue(
                verified["receipt"]["service_result"]["recovered"]
            )
            extra = cold_cache_root / "unsealed-extra"
            extra.mkdir()
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "exactly one regular receipt",
            ):
                self.scorecard.Campaign.verify_cold_cache_root(
                    *cold_cache_input
                )
            extra.rmdir()
            root_link = root / "cold-cache-link"
            root_link.symlink_to(cold_cache_root, target_is_directory=True)
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "cache root is absent",
            ):
                self.scorecard.Campaign.verify_cold_cache_root(
                    root_link,
                    cold_cache_input[1],
                )

            invalid = recovered
            invalid["service_result"]["removed_fixture_owned_paths"] = [
                "/eos/mpla-fixtures/s4-chain-sparse-v1/unowned"
            ]
            (
                cold_cache_root / self.scorecard.COLD_CACHE_RECEIPT_FILE
            ).write_text(json.dumps(invalid))
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "invalid recovery",
            ):
                self.scorecard.Campaign.verify_cold_cache_root(
                    *self.cold_cache_root_input(cold_cache_root)
                )

    def test_p8_cold_root_requires_pin_and_rejects_links_and_swap(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = pathlib.Path(temporary)
            cold_root = self.create_cold_cache_root(parent)
            cold_input = self.cold_cache_root_input(cold_root)

            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "external receipt pin mismatch",
            ):
                self.scorecard.Campaign.verify_cold_cache_root(
                    cold_root,
                    "0" * 64,
                )

            receipt_path = cold_root / self.scorecard.COLD_CACHE_RECEIPT_FILE
            hardlink = parent / "cold-receipt-hardlink"
            os.link(receipt_path, hardlink)
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "not one singly-linked regular file",
            ):
                self.scorecard.Campaign.verify_cold_cache_root(*cold_input)
            hardlink.unlink()

            original_capture = self.scorecard.open_relative_regular_once

            def replace_after_capture(root_descriptor, relative, label):
                payload, identity = original_capture(
                    root_descriptor,
                    relative,
                    label,
                )
                replacement = cold_root / "replacement-cold-receipt.json"
                replacement.write_bytes(payload)
                os.replace(replacement, receipt_path)
                return payload, identity

            with mock.patch.object(
                self.scorecard,
                "open_relative_regular_once",
                side_effect=replace_after_capture,
            ):
                with self.assertRaisesRegex(
                    self.scorecard.CampaignError,
                    "changed after receipt capture|changed during receipt capture",
                ):
                    self.scorecard.Campaign.verify_cold_cache_root(*cold_input)

            link_root = parent / "cold-link-root"
            link_root.mkdir()
            (link_root / self.scorecard.COLD_CACHE_RECEIPT_FILE).symlink_to(
                receipt_path
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "not safely readable",
            ):
                self.scorecard.Campaign.verify_cold_cache_root(
                    link_root,
                    cold_input[1],
                )

            swap_parent = parent / "root-swap"
            swap_parent.mkdir()
            swap_root = self.create_cold_cache_root(swap_parent)
            swap_input = self.cold_cache_root_input(swap_root)
            moved_root = swap_parent / "original-cold-cache"

            def replace_root_after_capture(root_descriptor, relative, label):
                payload, identity = original_capture(
                    root_descriptor,
                    relative,
                    label,
                )
                swap_root.rename(moved_root)
                swap_root.mkdir()
                (swap_root / relative).write_bytes(payload)
                return payload, identity

            with mock.patch.object(
                self.scorecard,
                "open_relative_regular_once",
                side_effect=replace_root_after_capture,
            ):
                with self.assertRaisesRegex(
                    self.scorecard.CampaignError,
                    (
                        "cache root changed during receipt capture"
                        "|root path changed during receipt capture"
                    ),
                ):
                    self.scorecard.Campaign.verify_cold_cache_root(*swap_input)

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
                self.scorecard.Campaign.verify_phase_root(
                    "fork",
                    partial,
                    "0" * 64,
                )

            corrupt = self.create_sealed_phase_root(root, "rollback")
            raw_result = (
                corrupt / "cases" / "BG-ROLLBACK" / "raw-result.json"
            )
            raw_result.write_text('{"corrupt":true}\n')
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "manifest verification failed for cases/BG-ROLLBACK/raw-result.json",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "rollback",
                    corrupt,
                    self.phase_root_input(corrupt)[1],
                )

            historical_receipt = self.create_sealed_phase_root(
                root,
                "activation",
            )
            (historical_receipt / "phase-receipt.json").write_text(
                '{"status":"PASS","tampered":true}\n'
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "manifest verification failed for phase-receipt.json",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "activation",
                    historical_receipt,
                    self.phase_root_input(historical_receipt)[1],
                )

            historical_artifacts = self.create_sealed_phase_root(root, "fork")
            staged_artifacts_path = historical_artifacts / "staged-artifacts.json"
            staged_artifacts = json.loads(staged_artifacts_path.read_text())
            staged_artifacts["artifacts"]["mpla-poc-oracle"]["sha256"] = (
                "0" * 64
            )
            staged_artifacts_path.write_text(json.dumps(staged_artifacts))
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "manifest verification failed for staged-artifacts.json",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "fork",
                    historical_artifacts,
                    self.phase_root_input(historical_artifacts)[1],
                )

            historical_blob = self.create_sealed_phase_root(
                root,
                "publication",
                sealed_artifact_blob=True,
            )
            (historical_blob / "artifacts" / "mpla-poc-oracle").write_bytes(
                b"tampered-oracle"
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "manifest verification failed for artifacts/mpla-poc-oracle",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "publication",
                    historical_blob,
                    self.phase_root_input(historical_blob)[1],
                )

            sealed_with_extra_directory = self.create_sealed_phase_root(
                root,
                "stream",
            )
            (sealed_with_extra_directory / "unsealed-empty").mkdir()
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "unsealed extra or missing entries",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "stream",
                    sealed_with_extra_directory,
                    self.phase_root_input(sealed_with_extra_directory)[1],
                )

    def test_p8_rejects_a_sealed_but_ineligible_phase_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for case in self.scorecard.SCORECARD_CASES:
                if case == "publication":
                    continue
                failed = self.create_sealed_phase_root(
                    root,
                    case,
                    receipt_status="FAIL",
                )
                with self.subTest(case=case):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "phase eligibility receipt is invalid",
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            case,
                            *self.phase_root_input(failed),
                        )

    def test_p8_accepts_only_publication_terminal_fail_and_reports_it(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            publication = self.create_sealed_phase_root(
                root,
                "publication",
                receipt_status="FAIL",
            )

            verified = self.scorecard.Campaign.verify_phase_root(
                "publication",
                *self.phase_root_input(publication),
            )

            self.assertEqual(verified["receipt"]["status"], "FAIL")
            self.assertTrue(verified["receipt"]["cap_pass"])
            self.assertTrue(verified["receipt"]["cleanup_pass"])
            self.assertFalse(verified["result"]["gate"]["required"])

            for name, overrides in (
                (
                    "decision_status",
                    {"decision_overrides": {"status": "PASS"}},
                ),
                (
                    "decision_gate",
                    {
                        "decision_overrides": {
                            "gates": {"BG-PUBLISH-SMALL": True},
                        }
                    },
                ),
            ):
                mismatch_parent = root / name
                mismatch_parent.mkdir()
                mismatch = self.create_sealed_phase_root(
                    mismatch_parent,
                    "publication",
                    receipt_status="FAIL",
                    **overrides,
                )
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "phase eligibility receipt is invalid",
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            "publication",
                            *self.phase_root_input(mismatch),
                        )

            p8_root = root / "p8-final-decision"
            p8_root.mkdir()
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-p4-terminal-fail-test",
                p8_root,
                "sealing",
            )
            results = {
                "qualification": {},
                "activation": {
                    "activate_exact_gate": {
                        "required": True,
                        "preferred": True,
                    },
                    "activate_same_gate": {
                        "required": True,
                        "preferred": True,
                    },
                },
                "fork": {"fork_gate": {"required": True, "preferred": True}},
                "rollback": {
                    "rollback_gate": {"required": True, "preferred": True}
                },
                "publication": {
                    "gate": {"required": False, "preferred": False}
                },
                "squash": {"squash_gate": {"required": True}},
                "stream": {"required": True},
                "recovery": {},
            }
            campaign.correctness_report = lambda _results, _cleanup: {
                "checks": {"sealed_phase_correctness": "PASS"}
            }
            campaign.result_correctness = lambda _results, _cleanup: True
            campaign.security_profile_report = lambda _results: {"status": "PASS"}
            campaign.resources_report = lambda _results: {"status": "PASS"}
            artifact_path = p8_root / "artifacts" / "mpla-speed-poc-v1"
            artifact_path.parent.mkdir()
            artifact_path.write_bytes(b"sealed-p8-coordinator")
            artifact_identity = {
                "bytes": artifact_path.stat().st_size,
                "sha256": self.scorecard.sha256_file(artifact_path),
                "sealed_path": "artifacts/mpla-speed-poc-v1",
                "verification_level": "sealed-bytes",
            }
            self.scorecard.write_new_json(
                p8_root / "phase-inputs.json",
                {
                    "P8-ARTIFACT": artifact_identity,
                    "lineage": {"p8_artifact": artifact_identity},
                },
            )
            campaign.p8_artifact_identity = dict(artifact_identity)

            final = campaign.finalize_evidence(results, True)

            self.assertEqual(final["decision"]["POC_CORRECTNESS"], "PASS")
            self.assertEqual(final["decision"]["POC_100X"], "NOT_SUPPORTED")
            self.assertEqual(final["decision"]["POC_500X"], "NOT_SUPPORTED")

    def test_p8_external_pin_rejects_resealed_historical_tamper(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            activation = self.create_sealed_phase_root(root, "activation")
            original_pin = self.phase_root_input(activation)[1]
            receipt_path = activation / "phase-receipt.json"
            receipt = json.loads(receipt_path.read_text())
            receipt["failures"] = ["historical receipt was rewritten"]
            receipt_path.write_text(json.dumps(receipt))
            (activation / "manifest.sha256").unlink()
            (activation / "manifest-verification.json").unlink()
            replacement_pin = self.scorecard.Campaign(
                "phase-root-reseal-test",
                activation,
                "activation",
            ).seal_manifest()
            self.assertNotEqual(original_pin, replacement_pin)

            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "external manifest pin mismatch",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "activation",
                    activation,
                    original_pin,
                )

    def test_p8_rejects_manifest_path_and_phase_root_entry_confusion(self) -> None:
        malformed_entries = {
            "parent": f"{'a' * 64}  ../escape\n",
            "absolute": f"{'a' * 64}  /tmp/escape\n",
            "duplicate": None,
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for name, content in malformed_entries.items():
                case_parent = root / name
                case_parent.mkdir()
                phase_root = self.create_sealed_phase_root(case_parent, "fork")
                manifest_path = phase_root / "manifest.sha256"
                if content is None:
                    first_line = manifest_path.read_text().splitlines()[0]
                    content = f"{first_line}\n{first_line}\n"
                manifest_path.write_text(content)
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "manifest entry is malformed",
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            "fork",
                            phase_root,
                            self.scorecard.sha256_file(manifest_path),
                        )

            target_parent = root / "symlink-target"
            target_parent.mkdir()
            target = self.create_sealed_phase_root(target_parent, "rollback")
            target_pin = self.phase_root_input(target)[1]
            root_link = root / "rollback-link"
            root_link.symlink_to(target, target_is_directory=True)
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "phase root is absent",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "rollback",
                    root_link,
                    target_pin,
                )

            special_root = root / "special-root"
            os.mkfifo(special_root)
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "phase root is absent",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "rollback",
                    special_root,
                    target_pin,
                )

            special_parent = root / "special-entry"
            special_parent.mkdir()
            special = self.create_sealed_phase_root(special_parent, "squash")
            special_pin = self.phase_root_input(special)[1]
            os.mkfifo(special / "unsealed-fifo")
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "contains a special entry",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "squash",
                    special,
                    special_pin,
                )

    def test_p8_rejects_sealed_artifact_path_confusion(self) -> None:
        verified, current_source = self.preserved_lineage_inputs()
        current_identity = verified["recovery"]["staged_artifacts"]["artifacts"][
            "mpla-speed-poc-v1"
        ]
        current_coordinator = (
            current_identity["bytes"],
            current_identity["sha256"],
        )
        identity = verified["qualification"]["staged_artifacts"]["artifacts"][
            "mpla-poc-oracle"
        ]
        with tempfile.TemporaryDirectory() as temporary:
            artifact_root = pathlib.Path(temporary)
            for name, sealed_path in (
                ("wrong_type", 7),
                ("null", None),
                ("empty", ""),
                ("traversal", "../mpla-poc-oracle"),
                ("absolute", "/tmp/mpla-poc-oracle"),
            ):
                malformed = copy.deepcopy(verified)
                malformed["qualification"]["root"] = str(artifact_root)
                malformed["qualification"]["staged_artifacts"]["artifacts"][
                    "mpla-poc-oracle"
                ] = {**identity, "sealed_path": sealed_path}
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "sealed path is malformed",
                    ):
                        self.scorecard.verify_phase_lineage(
                            malformed,
                            current_source,
                            current_coordinator,
                        )

        with tempfile.TemporaryDirectory() as temporary:
            artifact_root = pathlib.Path(temporary)
            target = artifact_root / "oracle-target"
            target.write_bytes(b"x")
            link = artifact_root / "oracle-link"
            link.symlink_to(target)
            malformed = copy.deepcopy(verified)
            malformed["qualification"]["root"] = str(artifact_root)
            malformed["qualification"]["staged_artifacts"]["artifacts"][
                "mpla-poc-oracle"
            ] = {
                "bytes": 1,
                "sha256": self.scorecard.sha256_file(target),
                "sealed_path": link.name,
            }
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "sealed blob is invalid",
            ):
                self.scorecard.verify_phase_lineage(
                    malformed,
                    current_source,
                    current_coordinator,
                )

    def test_p7_requires_present_and_correct_sealed_artifact_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for mode in ("missing", "corrupt"):
                case_parent = root / mode
                case_parent.mkdir()
                phase_root = self.create_sealed_phase_root(
                    case_parent,
                    "recovery",
                )
                staged_path = phase_root / "staged-artifacts.json"
                staged = json.loads(staged_path.read_text())
                coordinator = staged["artifacts"]["mpla-speed-poc-v1"]
                if mode == "missing":
                    coordinator.pop("sealed_path")
                    expected = "requires sealed_path"
                else:
                    coordinator["sealed_path"] = "artifacts/mpla-poc-oracle"
                    expected = "sealed blob is invalid"
                staged_path.write_text(json.dumps(staged) + "\n")
                phase_input = self.reseal_phase_root(phase_root, "recovery")
                with self.subTest(mode=mode):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        expected,
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            "recovery",
                            *phase_input,
                        )

    def test_p8_final_manifest_rejects_artifact_and_phase_input_substitution(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            evidence_root = pathlib.Path(temporary)
            artifact_path = evidence_root / "artifacts" / "mpla-speed-poc-v1"
            artifact_path.parent.mkdir()
            artifact_path.write_bytes(b"sealed-p8-coordinator")
            identity = {
                "bytes": artifact_path.stat().st_size,
                "sha256": self.scorecard.sha256_file(artifact_path),
                "sealed_path": "artifacts/mpla-speed-poc-v1",
                "verification_level": "sealed-bytes",
            }
            phase_inputs_path = evidence_root / "phase-inputs.json"
            phase_inputs = {
                "P8-ARTIFACT": dict(identity),
                "lineage": {"p8_artifact": dict(identity)},
            }
            phase_inputs_path.write_text(json.dumps(phase_inputs) + "\n")
            campaign = self.scorecard.Campaign(
                "p8-artifact-binding-test",
                evidence_root,
                "sealing",
            )
            campaign.p8_artifact_identity = dict(identity)

            mismatched_inputs = copy.deepcopy(phase_inputs)
            mismatched_inputs["P8-ARTIFACT"]["sha256"] = "0" * 64
            phase_inputs_path.write_text(json.dumps(mismatched_inputs) + "\n")
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "not identical across lineage and phase-inputs",
            ):
                campaign.p8_manifest_artifact_binding()

            phase_inputs_path.write_text(json.dumps(phase_inputs) + "\n")
            expected_manifest_entries = campaign.p8_manifest_artifact_binding()
            phase_inputs_path.write_text(
                json.dumps(phase_inputs, indent=2, sort_keys=True) + "\n"
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "final manifest artifact digest does not match the P8 lineage",
            ):
                campaign.seal_manifest(expected_manifest_entries)

            phase_inputs_path.write_text(json.dumps(phase_inputs) + "\n")
            artifact_path.write_bytes(b"substituted-after-binding")
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "final manifest artifact digest does not match the P8 lineage",
            ):
                campaign.seal_manifest(expected_manifest_entries)

    def test_p8_rejects_receipt_types_and_phase_local_cap_forgery(self) -> None:
        cases = (
            (
                "elapsed_bool",
                "fork",
                {"receipt_overrides": {"elapsed_wall_ns": True}},
            ),
            (
                "receipt_schema_bool",
                "activation",
                {"receipt_overrides": {"schema_version": True}},
            ),
            (
                "declaration_schema_bool",
                "fork",
                {"declaration_overrides": {"schema_version": True}},
            ),
            (
                "multiplier_float",
                "rollback",
                {"receipt_overrides": {"selected_multiplier_milli": 2_000.0}},
            ),
            (
                "clock_type",
                "rollback",
                {"receipt_overrides": {"clock": 7}},
            ),
            (
                "decision_gate_integer",
                "fork",
                {"decision_overrides": {"gates": {"BG-FORK": 1}}},
            ),
            (
                "receipt_run_id_mismatch",
                "publication",
                {"receipt_overrides": {"run_id": "different-phase-run"}},
            ),
            (
                "environment_run_id_type",
                "squash",
                {"environment_overrides": {"run_id": 7}},
            ),
            (
                "publication_failed_cleanup",
                "publication",
                {
                    "receipt_status": "FAIL",
                    "receipt_overrides": {"cleanup_pass": False},
                },
            ),
            (
                "aggregate_cannot_mask_phase_overrun",
                "stream",
                {
                    "receipt_overrides": {
                        "elapsed_wall_ns": (
                            self.scorecard.CASE_DEADLINES_SECONDS["stream"]
                            * 1_000_000_000
                            + 1
                        ),
                        "total_harness_wall_ns": (
                            self.scorecard.CASE_DEADLINES_SECONDS["stream"]
                            * 1_000_000_000
                            + 2
                        ),
                        "cap_pass": True,
                    }
                },
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for name, case, kwargs in cases:
                case_parent = root / name
                case_parent.mkdir()
                phase_root = self.create_sealed_phase_root(
                    case_parent,
                    case,
                    **kwargs,
                )
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "phase eligibility receipt is invalid",
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            case,
                            *self.phase_root_input(phase_root),
                        )

    def test_p8_binds_raw_results_to_phase_run_build_and_runner(self) -> None:
        corruptions = (
            ("run_id", "qualification", {"run_id": "wrong-run"}),
            ("build_commit", "publication", {"build_commit": "0" * 40}),
            ("historical_optional_phase", "activation", {"phase": "fork"}),
            ("runner", "squash", {"runner": "mpla_fork_scorecard"}),
            ("p7_phase", "recovery", {"phase": "stream"}),
            (
                "p7_runner",
                "recovery",
                {"runner": "mpla_stream_scorecard"},
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for name, case, overrides in corruptions:
                case_parent = root / name
                case_parent.mkdir()
                phase_root = self.create_sealed_phase_root(case_parent, case)
                raw_paths = [
                    phase_root / "cases" / gate / "raw-result.json"
                    for gate in self.scorecard.CASE_GATES[case]
                ] or [phase_root / "cases" / case / "raw-result.json"]
                for raw_path in raw_paths:
                    result = json.loads(raw_path.read_text())
                    result.update(overrides)
                    raw_path.write_text(json.dumps(result) + "\n")
                phase_input = self.reseal_phase_root(phase_root, case)
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "raw phase result identity is invalid",
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            case,
                            *phase_input,
                        )

            for field in ("phase", "runner"):
                case_parent = root / f"p7-missing-{field}"
                case_parent.mkdir()
                phase_root = self.create_sealed_phase_root(
                    case_parent,
                    "recovery",
                )
                raw_path = phase_root / "cases" / "HV-07" / "raw-result.json"
                result = json.loads(raw_path.read_text())
                result.pop(field)
                raw_path.write_text(json.dumps(result) + "\n")
                phase_input = self.reseal_phase_root(phase_root, "recovery")
                with self.subTest(p7_missing=field):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "raw phase result identity is invalid",
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            "recovery",
                            *phase_input,
                        )

    def test_p8_reports_exact_historical_and_explicit_raw_bindings(self) -> None:
        self.assertEqual(
            self.scorecard.HISTORICAL_INDIRECT_RAW_RESULT_CASES,
            frozenset(
                {"qualification", "activation", "publication", "stream"}
            ),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for case in self.scorecard.SCORECARD_CASES:
                case_parent = root / f"binding-{case}"
                case_parent.mkdir()
                phase_root = self.create_sealed_phase_root(case_parent, case)
                verified = self.scorecard.Campaign.verify_phase_root(
                    case,
                    *self.phase_root_input(phase_root),
                )
                expected = self.scorecard.expected_raw_result_binding(case)
                self.assertEqual(verified["raw_result_binding"], expected)
                if case in self.scorecard.HISTORICAL_INDIRECT_RAW_RESULT_CASES:
                    self.assertEqual(
                        expected["mode"],
                        "historical-indirect-kind-and-sealed-receipts",
                    )
                else:
                    self.assertEqual(
                        expected["mode"],
                        "explicit-raw-phase-runner",
                    )

    def test_p8_rejects_non_boolean_preferred_results(self) -> None:
        corruptions = (
            ("activation", "activate_exact_gate", 1),
            ("fork", "fork_gate", "true"),
            ("rollback", "rollback_gate", None),
            ("publication", "gate", 1.0),
            ("stream", None, []),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for index, (case, container, invalid) in enumerate(corruptions):
                case_parent = root / f"preferred-{index}"
                case_parent.mkdir()
                phase_root = self.create_sealed_phase_root(case_parent, case)
                raw_paths = [
                    phase_root / "cases" / gate / "raw-result.json"
                    for gate in self.scorecard.CASE_GATES[case]
                ] or [phase_root / "cases" / case / "raw-result.json"]
                for raw_path in raw_paths:
                    result = json.loads(raw_path.read_text())
                    target = result if container is None else result[container]
                    target["preferred"] = invalid
                    raw_path.write_text(json.dumps(result) + "\n")
                phase_input = self.reseal_phase_root(phase_root, case)
                with self.subTest(case=case):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "preferred result is not an exact boolean",
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            case,
                            *phase_input,
                        )

    def test_p8_rejects_phase_file_replacement_after_single_capture(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            phase_root = self.create_sealed_phase_root(root, "fork")
            phase_input = self.phase_root_input(phase_root)
            original = self.scorecard.open_relative_regular_once
            captured_paths = []

            def replace_after_capture(root_descriptor, relative, label):
                payload, identity = original(root_descriptor, relative, label)
                captured_paths.append(relative)
                if relative == "phase-receipt.json":
                    replacement = phase_root / "replacement-receipt.json"
                    replacement.write_bytes(payload)
                    os.replace(replacement, phase_root / relative)
                return payload, identity

            with mock.patch.object(
                self.scorecard,
                "open_relative_regular_once",
                side_effect=replace_after_capture,
            ):
                with self.assertRaisesRegex(
                    self.scorecard.CampaignError,
                    "changed after evidence capture|changed during evidence capture",
                ):
                    self.scorecard.Campaign.verify_phase_root(
                        "fork",
                        *phase_input,
                    )
            self.assertEqual(len(captured_paths), len(set(captured_paths)))

    def test_p8_rejects_nested_directory_mutation_after_inventory_visit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            phase_root = self.create_sealed_phase_root(root, "fork")
            phase_input = self.phase_root_input(phase_root)
            nested = phase_root / "cases" / "BG-FORK"
            nested_stat = nested.stat()
            nested_identity = (nested_stat.st_dev, nested_stat.st_ino)
            original_inventory = self.scorecard.inventory_phase_root
            original_close = os.close
            mutated = False

            def close_and_mutate(descriptor):
                nonlocal mutated
                try:
                    details = os.fstat(descriptor)
                    identity = (details.st_dev, details.st_ino)
                except OSError:
                    identity = None
                original_close(descriptor)
                if identity == nested_identity and not mutated:
                    mutated = True
                    (nested / "concurrent-unsealed-entry").write_bytes(b"changed")

            def inventory_with_nested_mutation(root_descriptor, case):
                with mock.patch.object(
                    self.scorecard.os,
                    "close",
                    side_effect=close_and_mutate,
                ):
                    return original_inventory(root_descriptor, case)

            with mock.patch.object(
                self.scorecard,
                "inventory_phase_root",
                side_effect=inventory_with_nested_mutation,
            ):
                with self.assertRaisesRegex(
                    self.scorecard.CampaignError,
                    "changed after inventory at cases/BG-FORK",
                ):
                    self.scorecard.Campaign.verify_phase_root(
                        "fork",
                        *phase_input,
                    )
            self.assertTrue(mutated)

    def test_p8_rejects_phase_root_path_replacement_after_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = pathlib.Path(temporary)
            phase_root = self.create_sealed_phase_root(parent, "fork")
            phase_input = self.phase_root_input(phase_root)
            moved_root = parent / "original-fork-phase"
            original_inventory = self.scorecard.inventory_phase_root

            def inventory_and_replace_root(root_descriptor, case):
                inventory = original_inventory(root_descriptor, case)
                phase_root.rename(moved_root)
                phase_root.mkdir()
                return inventory

            with mock.patch.object(
                self.scorecard,
                "inventory_phase_root",
                side_effect=inventory_and_replace_root,
            ):
                with self.assertRaisesRegex(
                    self.scorecard.CampaignError,
                    "phase root path changed during evidence capture",
                ):
                    self.scorecard.Campaign.verify_phase_root(
                        "fork",
                        *phase_input,
                    )

    def test_p8_rejects_non_object_receipt_and_environment(self) -> None:
        corruptions = (
            ("receipt", "phase-receipt.json", []),
            ("environment", "environment.json", []),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for name, filename, value in corruptions:
                case_parent = root / name
                case_parent.mkdir()
                phase_root = self.create_sealed_phase_root(
                    case_parent,
                    "fork",
                )
                (phase_root / filename).write_text(json.dumps(value) + "\n")
                phase_input = self.reseal_phase_root(phase_root, "fork")
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        "phase eligibility receipt is invalid",
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            "fork",
                            *phase_input,
                        )

    def test_p8_rejects_non_boolean_raw_gate_and_schema(self) -> None:
        corruptions = (
            ("gate", ("fork_gate", "required"), 1, "not an exact boolean"),
            ("schema", ("schema_version",), True, "raw phase result schema"),
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for name, path, value, message in corruptions:
                case_parent = root / name
                case_parent.mkdir()
                phase_root = self.create_sealed_phase_root(
                    case_parent,
                    "fork",
                )
                raw_path = phase_root / "cases" / "BG-FORK" / "raw-result.json"
                result = json.loads(raw_path.read_text())
                target = result
                for field in path[:-1]:
                    target = target[field]
                target[path[-1]] = value
                raw_path.write_text(json.dumps(result) + "\n")
                phase_input = self.reseal_phase_root(phase_root, "fork")
                with self.subTest(name=name):
                    with self.assertRaisesRegex(
                        self.scorecard.CampaignError,
                        message,
                    ):
                        self.scorecard.Campaign.verify_phase_root(
                            "fork",
                            *phase_input,
                        )

    def test_p8_rejects_non_utc_timestamp_and_duplicate_run_id(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            timestamp_parent = root / "timestamp"
            timestamp_parent.mkdir()
            invalid_timestamp = self.create_sealed_phase_root(
                timestamp_parent,
                "fork",
                environment_overrides={
                    "created_utc": "2026-07-30T00:02:00",
                },
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "environment timestamp is not a UTC timestamp",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "fork",
                    *self.phase_root_input(invalid_timestamp),
                )

            declaration_parent = root / "declaration-timestamp"
            declaration_parent.mkdir()
            invalid_declaration = self.create_sealed_phase_root(
                declaration_parent,
                "rollback",
                declaration_overrides={
                    "declared_utc": "2026-07-30T00:03:00",
                },
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "phase declaration timestamp is not a UTC timestamp",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "rollback",
                    *self.phase_root_input(invalid_declaration),
                )

            chronology_parent = root / "within-phase-chronology"
            chronology_parent.mkdir()
            reversed_timestamps = self.create_sealed_phase_root(
                chronology_parent,
                "publication",
                created_utc_override="2026-07-30T00:04:01+00:00",
                declaration_overrides={
                    "declared_utc": "2026-07-30T00:04:00+00:00",
                },
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "phase timestamps are not chronological",
            ):
                self.scorecard.Campaign.verify_phase_root(
                    "publication",
                    *self.phase_root_input(reversed_timestamps),
                )

            phase_parent = root / "duplicate-run-id"
            phase_parent.mkdir()
            phase_paths = {}
            for case in self.scorecard.SCORECARD_CASES:
                run_id_override = (
                    "phase-root-test-squash" if case == "stream" else None
                )
                phase_paths[case] = self.create_sealed_phase_root(
                    phase_parent,
                    case,
                    run_id_override=run_id_override,
                )
            phase_roots = {
                case: self.phase_root_input(phase_root)
                for case, phase_root in phase_paths.items()
            }
            cold_cache_root = self.create_cold_cache_root(root)
            p8_root = root / "duplicate-run-id-p8"
            p8_root.mkdir()
            self.scorecard.write_new_json(
                p8_root / "source.json",
                {
                    "commit": self.scorecard.BUILD_COMMIT,
                    "tree": "1" * 40,
                    "tracked_diff_sha256": self.scorecard.sha256_bytes(b""),
                    "tracked_diff_bytes": 0,
                    "porcelain_sha256": self.scorecard.sha256_bytes(b""),
                    "porcelain": "",
                    "worktree_files": {},
                },
            )
            campaign = self.scorecard.Campaign(
                "mpla-scorecard-duplicate-phase-run-id-test",
                p8_root,
                "sealing",
                phase_roots,
                self.cold_cache_root_input(cold_cache_root),
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "unique sequential phase creation",
            ):
                campaign.load_phase_results()

            order_root = root / "out-of-order"
            order_root.mkdir()
            order_phase_parent = order_root / "phases"
            order_phase_parent.mkdir()
            order_paths = {
                case: self.create_sealed_phase_root(
                    order_phase_parent,
                    case,
                    created_utc_override=(
                        "2026-07-30T00:04:30+00:00"
                        if case == "stream"
                        else None
                    ),
                )
                for case in self.scorecard.SCORECARD_CASES
            }
            order_roots = {
                case: self.phase_root_input(phase_root)
                for case, phase_root in order_paths.items()
            }
            order_cold_cache_root = self.create_cold_cache_root(order_root)
            order_p8_root = order_root / "p8"
            order_p8_root.mkdir()
            self.scorecard.write_new_json(
                order_p8_root / "source.json",
                {
                    "commit": self.scorecard.BUILD_COMMIT,
                    "tree": "1" * 40,
                    "tracked_diff_sha256": self.scorecard.sha256_bytes(b""),
                    "tracked_diff_bytes": 0,
                    "porcelain_sha256": self.scorecard.sha256_bytes(b""),
                    "porcelain": "",
                    "worktree_files": {},
                },
            )
            p8_run_id = "mpla-scorecard-out-of-order-phase-test"
            p8_created_utc = "2026-07-30T00:08:00+00:00"
            self.scorecard.write_new_json(
                order_p8_root / "environment.json",
                {
                    "run_id": p8_run_id,
                    "case": "sealing",
                    "image": self.scorecard.IMAGE,
                    "platform": "linux/arm64",
                    "gateway_socket": self.scorecard.GATEWAY_SOCKET,
                    "gateway_config": str(self.scorecard.M3_SCORECARD_CONFIG),
                    "r0": str(self.scorecard.R0),
                    "build_commit": self.scorecard.BUILD_COMMIT,
                    "created_utc": p8_created_utc,
                },
            )
            self.scorecard.write_new_json(
                order_p8_root / "phase-declaration.json",
                {
                    "schema_version": 1,
                    "kind": "mpla_booster_phase_declaration_v1",
                    "run_id": p8_run_id,
                    "phase": "sealing",
                    "runner": self.scorecard.CASE_RUNNERS["sealing"],
                    "suggested_budget_seconds": (
                        self.scorecard.CASE_SUGGESTED_BUDGET_SECONDS["sealing"]
                    ),
                    "selected_multiplier_milli": (
                        self.scorecard.CASE_DEADLINES_SECONDS["sealing"]
                        * 1_000
                        // self.scorecard.CASE_SUGGESTED_BUDGET_SECONDS[
                            "sealing"
                        ]
                    ),
                    "calculated_phase_cap_seconds": (
                        self.scorecard.CASE_DEADLINES_SECONDS["sealing"]
                    ),
                    "deadline_carryover_seconds": 0,
                    "bounded_work": (
                        "sealed P0-P7 receipt and hash verification only"
                    ),
                    "declared_utc": "2026-07-30T00:08:01+00:00",
                },
            )
            with self.assertRaisesRegex(
                self.scorecard.CampaignError,
                "unique sequential phase creation",
            ):
                self.scorecard.Campaign(
                    "mpla-scorecard-out-of-order-phase-test",
                    order_p8_root,
                    "sealing",
                    order_roots,
                    self.cold_cache_root_input(order_cold_cache_root),
                ).load_phase_results()

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
            campaign.sandbox_ids = [
                "eos-candidate",
                "eos-coordinator",
                "eos-control-1",
                "eos-control-2",
                "eos-control-3",
            ]
            campaign.candidate_sandbox_id = "eos-candidate"
            campaign.coordinator_sandbox_id = "eos-coordinator"
            campaign.control_sandbox_ids = [
                "eos-control-1",
                "eos-control-2",
                "eos-control-3",
            ]
            campaign.workspace_session_id = "workspace-session"
            runtime_calls = []
            campaign.runtime = lambda *args, **kwargs: runtime_calls.append(
                (args, kwargs)
            ) or {"status": "destroyed"}
            campaign.manager = lambda *args, **kwargs: {"status": "destroyed"}

            campaign.cleanup_execution("publication")

            self.assertEqual(campaign.sandbox_ids, [])
            self.assertIsNone(campaign.candidate_sandbox_id)
            self.assertIsNone(campaign.coordinator_sandbox_id)
            self.assertEqual(campaign.control_sandbox_ids, [])
            self.assertIsNone(campaign.workspace_session_id)
            self.assertEqual(runtime_calls[0][0][0], "eos-coordinator")
            self.assertEqual(
                campaign.cleanup["case_cleanup"],
                [
                    {
                        "case": "publication",
                        "workspace_session_id": "workspace-session",
                        "sandbox_ids": [
                            "eos-candidate",
                            "eos-coordinator",
                            "eos-control-1",
                            "eos-control-2",
                            "eos-control-3",
                        ],
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
                "recovery": self.hv07_result(),
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

    def test_hv07_validation_requires_every_exact_physical_point(self) -> None:
        valid = self.hv07_result()
        self.assertEqual(self.scorecard.validate_hv07_result(valid), [])

        missing = copy.deepcopy(valid)
        missing["fresh_sweep"]["points"].pop()
        self.assertTrue(self.scorecard.validate_hv07_result(missing))

        validation_only = copy.deepcopy(valid)
        replay = validation_only["fresh_sweep"]["points"][0]["details"]["record"][
            "observation"
        ]["recovery_replay_witness"]
        replay["recovery_invoked"] = False
        self.assertTrue(self.scorecard.validate_hv07_result(validation_only))

        resumed = copy.deepcopy(valid)
        resumed["fresh_sweep"]["points"][0]["details"]["record"]["observation"][
            "post_sealing_session_resumed"
        ] = True
        self.assertTrue(self.scorecard.validate_hv07_result(resumed))

        swapped_point = copy.deepcopy(valid)
        swapped_observation = swapped_point["fresh_sweep"]["points"][0][
            "details"
        ]["record"]["observation"]
        swapped_observation["physical_kill_witness"]["fault_point"] = (
            self.scorecard.HV07_FAULT_POINTS[1]
        )
        self.assertTrue(self.scorecard.validate_hv07_result(swapped_point))

        mismatched_operation = copy.deepcopy(valid)
        mismatched_observation = mismatched_operation["fresh_sweep"]["points"][0][
            "details"
        ]["record"]["observation"]
        mismatched_observation["real_operation_witness"]["operation_id"] = (
            "different-operation"
        )
        self.assertTrue(self.scorecard.validate_hv07_result(mismatched_operation))

        wrong_visibility = copy.deepcopy(valid)
        wrong_observation = wrong_visibility["fresh_sweep"]["points"][0][
            "details"
        ]["record"]["observation"]
        wrong_observation["recovery_replay_witness"]["selected_visibility"] = (
            "complete_new"
            if wrong_observation["selected_visibility"] == "old"
            else "old"
        )
        self.assertTrue(self.scorecard.validate_hv07_result(wrong_visibility))

        for field, value in (
            ("stationary_payload_path_before", None),
            ("stationary_payload_path_after", ""),
        ):
            invalid_stationary_path = copy.deepcopy(valid)
            invalid_stationary_path["fresh_sweep"]["points"][0]["details"][
                "record"
            ]["observation"]["real_operation_witness"][field] = value
            self.assertTrue(
                self.scorecard.validate_hv07_result(invalid_stationary_path)
            )

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

    def test_squash_boundary_requires_pre_phase_setup_and_exact_samples(
        self,
    ) -> None:
        result_sha256 = "a" * 64
        run_id = "mpla-squash-boundary-test"
        roots = {
            "root_id": "11" * 32,
            "attribution_root_id": "22" * 32,
        }
        samples = [
            {
                "operation": "squash_mpla_branch",
                "request_id": f"{run_id}-squash-{sample:02}",
                "outer_elapsed_ns": 2_000_000 + sample * 100_000,
                "response": {
                    "run_id": run_id,
                    "branch": "main",
                    "roots": roots,
                    "ref_sequence": sample + 2,
                    "service_elapsed_ns": 200_000 + sample * 10_000,
                    "lifecycle": {
                        "operation_id": f"{run_id}-squash-{sample:02}",
                        "committed": True,
                        "idempotent_replay": False,
                        "selected_ref": (
                            f"main@{sample + 2}#"
                            f"{format(sample + 11, 'x') * 64}"
                        ),
                        "service_elapsed_ns": 200_000
                        + sample * 10_000,
                    },
                },
            }
            for sample in range(3)
        ]
        receipts = [
            {
                "sample": sample,
                "operation": "squash_mpla_branch",
                "request_id": invocation["request_id"],
                "outer_elapsed_ns": invocation["outer_elapsed_ns"],
                "service_elapsed_ns": invocation["response"][
                    "service_elapsed_ns"
                ],
                "selected_ref": invocation["response"]["lifecycle"][
                    "selected_ref"
                ],
                "roots": invocation["response"]["roots"],
                "ref_sequence": invocation["response"]["ref_sequence"],
                "full_response_sha256": self.scorecard.canonical_json_sha256(
                    invocation["response"]
                ),
            }
            for sample, invocation in enumerate(samples)
        ]
        result = {
            "run_id": run_id,
            "candidate_prepared_before_phase": True,
            "squash_setup_elapsed_ns": 23_000_000_000,
            "phase_timing": {
                "elapsed_ns": 6_300_000,
                "measurement_scope": (
                    "exact sum of three public squash outer spans; "
                    "durable journal syncs excluded"
                ),
            },
            "baseline_activation": {
                "selected_ref": f"main@1#{'a' * 64}",
                "projection": {"roots": roots},
            },
            "squash_samples": samples,
            "squash_sample_receipts": receipts,
            "identity_and_attribution_stable": True,
            "public_outcomes_exact": True,
            "selected_ref_progression_exact": True,
            "squash_gate": {
                "gate": "AG-SQUASH",
                "outer_ns": [2_000_000, 2_100_000, 2_200_000],
                "service_ns": [200_000, 210_000, 220_000],
                "required": True,
            },
        }
        events = [
            {
                "schema_version": 1,
                "kind": "mpla_booster_squash_progress_v1",
                "stage": "started",
                "details": {},
            },
            {
                "schema_version": 1,
                "kind": "mpla_booster_squash_progress_v1",
                "stage": "squash_setup_completed_before_phase",
                "details": {"setup_elapsed_ns": 23_000_000_000},
            },
            *[
                {
                "schema_version": 1,
                "kind": "mpla_booster_squash_progress_v1",
                "stage": "squash_completed",
                "details": receipts[sample],
            }
                for sample in range(3)
            ],
            {
                "schema_version": 1,
                "kind": "mpla_booster_squash_progress_v1",
                "stage": "completed",
                "details": {"result_sha256": result_sha256},
            },
        ]

        def progress(value: list[dict]) -> dict:
            return {
                "file_read": {
                    "content": "\n".join(json.dumps(event) for event in value)
                }
            }

        self.scorecard.Campaign.validate_squash_phase_boundary(
            result,
            progress(events),
            result_sha256,
        )

        def reseal_receipt(corrupt: dict, sample_number: int) -> None:
            response = corrupt["squash_samples"][sample_number]["response"]
            receipt = corrupt["squash_sample_receipts"][sample_number]
            receipt["selected_ref"] = response["lifecycle"]["selected_ref"]
            receipt["roots"] = response["roots"]
            receipt["ref_sequence"] = response["ref_sequence"]
            receipt["service_elapsed_ns"] = response["service_elapsed_ns"]
            receipt["full_response_sha256"] = (
                self.scorecard.canonical_json_sha256(response)
            )

        changed_root = copy.deepcopy(result)
        changed_root["squash_samples"][1]["response"]["roots"][
            "root_id"
        ] = "44" * 32
        reseal_receipt(changed_root, 1)

        changed_attribution = copy.deepcopy(result)
        changed_attribution["squash_samples"][1]["response"]["roots"][
            "attribution_root_id"
        ] = "55" * 32
        reseal_receipt(changed_attribution, 1)

        skipped_revision = copy.deepcopy(result)
        skipped_response = skipped_revision["squash_samples"][1]["response"]
        skipped_response["ref_sequence"] = 4
        skipped_response["lifecycle"][
            "selected_ref"
        ] = f"main@4#{'e' * 64}"
        reseal_receipt(skipped_revision, 1)

        replayed = copy.deepcopy(result)
        replayed["squash_samples"][1]["response"]["lifecycle"][
            "idempotent_replay"
        ] = True
        reseal_receipt(replayed, 1)

        wrong_operation_id = copy.deepcopy(result)
        wrong_operation_id["squash_samples"][1]["response"]["lifecycle"][
            "operation_id"
        ] = "wrong-operation"
        reseal_receipt(wrong_operation_id, 1)

        malformed_ref = copy.deepcopy(result)
        malformed_ref["squash_samples"][1]["response"]["lifecycle"][
            "selected_ref"
        ] = f"other@3#{'c' * 64}"
        reseal_receipt(malformed_ref, 1)

        raw_envelope_drift = copy.deepcopy(result)
        raw_envelope_drift["squash_samples"][1][
            "request_id"
        ] = "wrong-raw-request"

        oversized_timing = copy.deepcopy(result)
        oversized_timing["squash_samples"][1][
            "outer_elapsed_ns"
        ] = 10_000_001
        oversized_timing["squash_sample_receipts"][1][
            "outer_elapsed_ns"
        ] = 10_000_001
        oversized_timing["squash_gate"]["outer_ns"][1] = 10_000_001
        oversized_timing["phase_timing"]["elapsed_ns"] = sum(
            oversized_timing["squash_gate"]["outer_ns"]
        )

        for name, corrupt_result in {
            "changed_root": changed_root,
            "changed_attribution": changed_attribution,
            "skipped_revision": skipped_revision,
            "replayed": replayed,
            "wrong_operation_id": wrong_operation_id,
            "malformed_ref": malformed_ref,
            "raw_envelope_drift": raw_envelope_drift,
            "oversized_timing": oversized_timing,
        }.items():
            with self.subTest(name=name):
                with self.assertRaises(self.scorecard.CampaignError):
                    self.scorecard.Campaign.validate_squash_result_boundary(
                        corrupt_result
                    )

        corruptions = {
            "prepared_in_phase": (
                {**result, "candidate_prepared_before_phase": False},
                events,
            ),
            "zero_setup": (
                {**result, "squash_setup_elapsed_ns": 0},
                events,
            ),
            "two_samples": (
                {**result, "squash_samples": [{}, {}]},
                events[:-2] + events[-1:],
            ),
            "setup_after_sample": (
                result,
                [events[0], events[2], events[1], *events[3:]],
            ),
            "wrong_result_hash": (
                result,
                events[:-1]
                + [
                    {
                        **events[-1],
                        "details": {"result_sha256": "b" * 64},
                    }
                ],
            ),
        }
        for name, (corrupt_result, corrupt_events) in corruptions.items():
            with self.subTest(name=name):
                with self.assertRaises(self.scorecard.CampaignError):
                    self.scorecard.Campaign.validate_squash_phase_boundary(
                        corrupt_result,
                        progress(corrupt_events),
                        result_sha256,
                    )

    def test_canonical_json_hash_matches_the_rust_nested_unicode_vector(
        self,
    ) -> None:
        self.assertEqual(
            self.scorecard.canonical_json_sha256(
                {
                    "z": 1,
                    "a": {
                        "β": "值",
                        "a": [True, None, 3],
                    },
                }
            ),
            "4863f8ef3b164d0b123602b5932e180d861402f1477f1b956c1766845fe671cc",
        )

    def test_stream_progress_path_and_capacity_budget_are_exact(self) -> None:
        self.assertEqual(
            self.scorecard.CASE_PROGRESS_FILES["stream"],
            "scorecard-stream-progress.jsonl",
        )
        self.assertEqual(self.scorecard.STREAM_CHANGED_UPPER_BYTES, 1024**3)
        self.assertEqual(
            self.scorecard.STREAM_REQUIRED_FREE_BYTES,
            self.scorecard.STREAM_CHANGED_UPPER_BYTES
            + self.scorecard.STREAM_DISK_MARGIN_BYTES,
        )
        self.assertGreaterEqual(
            self.scorecard.STREAM_DISK_MARGIN_BYTES * 5,
            self.scorecard.STREAM_CHANGED_UPPER_BYTES,
        )

    def test_stream_fixture_requires_four_ordered_64_mib_operations_per_file(
        self,
    ) -> None:
        operations = [
            {
                "offset": index * self.scorecard.STREAM_EXTENT_BYTES,
                "length": self.scorecard.STREAM_EXTENT_BYTES,
            }
            for index in range(self.scorecard.STREAM_EXTENTS_PER_FILE)
        ]
        files = [
            {
                "name": f"changed-upper-s2-{index:02}.bin",
                "logical_bytes": self.scorecard.STREAM_FILE_BYTES,
                "allocated_bytes": self.scorecard.STREAM_FILE_BYTES,
                "payload_sha256": (
                    "a6d72ac7690f53be6ae46ba88506bd97302a093f7108472bd9efc3cefda06484"
                ),
                "logical_extent_operations": copy.deepcopy(operations),
            }
            for index in range(self.scorecard.STREAM_FILE_COUNT)
        ]
        fixture = {
            "schema_version": 1,
            "kind": "mpla_s2_four_file_dense_deterministic_fixture_v1",
            "creation_method": "fallocate_zero_extents",
            "files": files,
            "logical_bytes": self.scorecard.STREAM_CHANGED_UPPER_BYTES,
            "data_bytes": self.scorecard.STREAM_CHANGED_UPPER_BYTES,
            "allocated_bytes": self.scorecard.STREAM_CHANGED_UPPER_BYTES,
            "deterministic": True,
            "sparse": False,
        }

        self.scorecard.Campaign.validate_stream_s2_fixture(fixture)

        corruptions = {}

        missing = copy.deepcopy(fixture)
        missing["files"][0].pop("logical_extent_operations")
        corruptions["missing"] = missing

        short = copy.deepcopy(fixture)
        short["files"][0]["logical_extent_operations"].pop()
        corruptions["short"] = short

        extra = copy.deepcopy(fixture)
        extra["files"][0]["logical_extent_operations"].append(
            {
                "offset": self.scorecard.STREAM_FILE_BYTES,
                "length": self.scorecard.STREAM_EXTENT_BYTES,
            }
        )
        corruptions["extra"] = extra

        wrong_offset = copy.deepcopy(fixture)
        wrong_offset["files"][0]["logical_extent_operations"][1]["offset"] += 1
        corruptions["wrong_offset"] = wrong_offset

        wrong_length = copy.deepcopy(fixture)
        wrong_length["files"][0]["logical_extent_operations"][1]["length"] -= 1
        corruptions["wrong_length"] = wrong_length

        reordered = copy.deepcopy(fixture)
        reordered["files"][0]["logical_extent_operations"].reverse()
        corruptions["reordered"] = reordered

        boolean_offset = copy.deepcopy(fixture)
        boolean_offset["files"][0]["logical_extent_operations"][0][
            "offset"
        ] = False
        corruptions["boolean_offset"] = boolean_offset

        extra_operation_field = copy.deepcopy(fixture)
        extra_operation_field["files"][0]["logical_extent_operations"][0][
            "untrusted"
        ] = 0
        corruptions["extra_operation_field"] = extra_operation_field

        for name, corrupt in corruptions.items():
            with self.subTest(name=name):
                with self.assertRaises(self.scorecard.CampaignError):
                    self.scorecard.Campaign.validate_stream_s2_fixture(corrupt)

    def test_stream_semantic_resource_maxima_fail_closed(self) -> None:
        maxima = {
            "application_pool_bytes": 8 * 1024 * 1024,
            "peak_managed_bytes": 8 * 1024 * 1024,
            "scan_window_bytes": 8 * 1024 * 1024,
            "spool_run_bytes": 4 * 1024 * 1024,
            "merge_fan_in": 8,
            "peak_open_data_fds": 16,
            "peak_data_workers": 4,
            "trie_fan_out": 1,
        }
        self.scorecard.Campaign.validate_stream_semantic_resource_maxima(maxima)
        for name, corrupt in {
            "managed_over_limit": {
                **maxima,
                "peak_managed_bytes": 8 * 1024 * 1024 + 1,
            },
            "worker_zero": {**maxima, "peak_data_workers": 0},
            "resealed_extra_field": {**maxima, "untrusted": 0},
        }.items():
            with self.subTest(name=name):
                with self.assertRaises(self.scorecard.CampaignError):
                    self.scorecard.Campaign.validate_stream_semantic_resource_maxima(
                        corrupt
                    )

    def test_stream_stationary_path_binding_uses_physical_witnesses(self) -> None:
        allocation_path = "/eos/layer-stack/mpla-poc/payload/allocations/aa/id"
        before = {
            "allocation_id": "allocation-id",
            "allocation_path": allocation_path,
            "logical_bytes": 1024**3,
        }
        after = copy.deepcopy(before)
        first = {
            "allocation_id": "allocation-id",
            "allocation_root": allocation_path,
            "physical": copy.deepcopy(before),
        }
        second = {
            "allocation_id": "allocation-id",
            "allocation_root": allocation_path,
            "physical": copy.deepcopy(after),
        }
        stationary = {
            "allocation_path_before": allocation_path,
            "allocation_path_after": allocation_path,
            "stable": {
                "allocation": {
                    "schema_version": 1,
                    "allocation_id": "allocation-id",
                    "created_by_operation": "activate",
                    "created_unix_ms": 1,
                },
            },
        }

        self.scorecard.Campaign.validate_stream_stationary_path_binding(
            stationary,
            before,
            after,
            first,
            second,
        )
        self.assertNotIn(
            "allocation_path",
            stationary["stable"]["allocation"],
        )

        corruptions = {
            "stationary_path": lambda values: values[0].update(
                allocation_path_before="/wrong"
            ),
            "before_path": lambda values: values[1].update(
                allocation_path="/wrong"
            ),
            "after_path": lambda values: values[2].update(
                allocation_path="/wrong"
            ),
            "first_root": lambda values: values[3].update(
                allocation_root="/wrong"
            ),
            "second_physical": lambda values: values[4].update(
                physical={"allocation_path": "/wrong"}
            ),
        }
        for name, corrupt in corruptions.items():
            values = copy.deepcopy(
                [stationary, before, after, first, second]
            )
            corrupt(values)
            with self.subTest(name=name):
                with self.assertRaises(self.scorecard.CampaignError):
                    self.scorecard.Campaign.validate_stream_stationary_path_binding(
                        *values
                    )

    def test_stream_capacity_reservation_is_dense(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / "reservation"
            receipt = self.scorecard.reserve_dense_file(path, 4096)
            self.assertEqual(receipt["logical_bytes"], 4096)
            self.assertGreaterEqual(receipt["allocated_bytes"], 4096)

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


class StableSourceIdentityRegressionTests(unittest.TestCase):
    def test_ignored_only_porcelain_normalizes_to_empty_string(self) -> None:
        import runpy

        module = runpy.run_path(
            str(pathlib.Path(__file__).with_name("run-mpla-booster-scorecard")),
            run_name="run_mpla_booster_scorecard_ignored_only_regression",
        )

        stable = module["stable_source_identity"](
            {
                "porcelain": "?? bin/__pycache__/ignored.pyc\n",
                "worktree_files": {
                    "bin/__pycache__/ignored.pyc": {"sha256": "0" * 64, "bytes": 1}
                },
            }
        )

        self.assertEqual(stable["porcelain"], "")
        self.assertEqual(stable["worktree_files"], {})


if __name__ == "__main__":
    unittest.main()
