#[allow(dead_code)]
mod scorecard {
    include!("../src/bin/mpla_poc_scorecard.rs");

    #[cfg(test)]
    mod tests {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        use clap::Parser;

        use super::*;

        #[derive(Debug, Parser)]
        struct TestCli {
            #[command(subcommand)]
            command: ScorecardCommand,
        }

        #[test]
        fn formal_gate_list_is_complete_and_ordered() {
            assert_eq!(
                FormalGate::ALL.map(FormalGate::as_str),
                [
                    "BG-ACTIVATE-EXACT",
                    "BG-ACTIVATE-SAME",
                    "BG-FORK",
                    "BG-ROLLBACK",
                    "BG-PUBLISH-SMALL",
                    "AG-SQUASH",
                    "AG-STREAM",
                ]
            );
        }

        #[test]
        fn gate_selector_parses_without_execution_by_default() {
            let cli = TestCli::try_parse_from([
                "scorecard",
                "gate",
                "--run-id",
                "booster-scorecard-20260729T003118Z",
                "--evidence-root",
                "/tmp/booster-scorecard-20260729T003118Z",
                "--interface-version",
                INTERFACE_VERSION,
                "--catalog-binding",
                "/tmp/catalog-binding.json",
                "--config",
                "/tmp/config.yml",
                "--image",
                PINNED_IMAGE,
                "--r0",
                EXACT_R0_PATH,
                "--lease-prefix",
                "booster-scorecard-20260729T003118Z-lease:",
                "--branch-prefix",
                "booster-scorecard-20260729T003118Z-branch",
                "--sandbox-prefix",
                "booster-scorecard-20260729T003118Z-sandbox",
                "--case",
                "BG-ACTIVATE-EXACT",
                "--samples",
                "3",
                "--command-timeout-ms",
                "600000",
                "--matched-control",
            ])
            .expect("gate arguments should parse");
            let TestCli {
                command: ScorecardCommand::Gate { capsule, execute },
            } = cli
            else {
                panic!("expected gate command");
            };
            assert_eq!(capsule.case, FormalGate::BgActivateExact);
            assert!(!execute);
        }

        #[test]
        fn historical_run_and_prefix_identities_are_refused() {
            assert!(validate_run_id(HISTORICAL_RUN_ID).is_err());
            assert!(validate_prefix("m2r-20260728T015724p0800:lead:", "lease").is_err());
            assert!(validate_prefix("m2r-lead", "branch").is_err());
            assert!(validate_prefix("heavy-main", "branch").is_err());
            assert!(validate_prefix("m2-lead", "evidence").is_err());
        }

        #[test]
        fn image_must_be_the_exact_pinned_stage_image() {
            assert!(validate_image("ubuntu:24.04").is_err());
            assert!(validate_image(
                "ubuntu@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            )
            .is_err());
            validate_image(PINNED_IMAGE).expect("specified Stage 04.6 image should pass");
        }

        #[test]
        fn generic_publish_and_squash_flags_do_not_satisfy_formal_mpla_gates() {
            let generic_facts = ControlCatalogFacts {
                publish_workspace_session: true,
                activate_workspace_session: true,
                fork_workspace_session: true,
                rollback_workspace_session: true,
                squash_layerstacks: true,
            };
            let no_formal_operations = FormalScorecardOperations {
                publish_mpla_workspace_session: false,
                squash_mpla_branch: false,
                exec_command: false,
            };
            assert!(require_catalog_operation(
                &generic_facts,
                &no_formal_operations,
                FormalGate::BgPublishSmall
            )
            .is_err());
            assert!(require_catalog_operation(
                &generic_facts,
                &no_formal_operations,
                FormalGate::AgSquash
            )
            .is_err());
            assert!(require_catalog_operation(
                &generic_facts,
                &no_formal_operations,
                FormalGate::AgStream
            )
            .is_err());
            require_catalog_operation(
                &generic_facts,
                &no_formal_operations,
                FormalGate::BgActivateExact,
            )
            .expect("generic activation is the bound public operation for this gate");
        }

        #[test]
        fn exact_formal_operation_names_match_the_public_runtime_routes() {
            assert_eq!(
                FormalGate::BgPublishSmall.required_catalog_operation(),
                "publish_mpla_workspace_session"
            );
            assert_eq!(
                FormalGate::AgSquash.required_catalog_operation(),
                "squash_mpla_branch"
            );
            assert_eq!(
                FormalGate::AgStream.required_catalog_operation(),
                "exec_command"
            );
        }

        #[test]
        fn request_ids_are_deterministic_bounded_and_arm_specific() {
            let candidate = deterministic_request_id(
                "booster-scorecard-unit-request",
                "BG-ACTIVATE-EXACT",
                "candidate",
                0,
            );
            assert_eq!(
                candidate,
                deterministic_request_id(
                    "booster-scorecard-unit-request",
                    "BG-ACTIVATE-EXACT",
                    "candidate",
                    0
                )
            );
            assert!(candidate.len() <= 128);
            assert_ne!(
                candidate,
                deterministic_request_id(
                    "booster-scorecard-unit-request",
                    "BG-ACTIVATE-EXACT",
                    "control",
                    0
                )
            );
        }

        #[test]
        fn r0_path_must_be_exact() {
            assert!(validate_r0(Path::new("/tmp/not-r0"), None).is_err());
            assert!(validate_r0(
                Path::new(
                    "/Users/yifanxu/Ephemeral-AI-Lab/experiment/materialization-benchmark-20260727"
                ),
                None,
            )
            .is_err());
        }

        #[test]
        fn preexisting_evidence_root_is_refused() {
            let run = validate_run_id("booster-scorecard-unit-existing").expect("valid run");
            let parent = unique_temp_dir("scorecard-existing");
            let root = parent.join(run.as_str());
            fs::create_dir(&root).expect("create existing root");
            assert!(validate_fresh_evidence_root(&root, &run).is_err());
            fs::remove_dir_all(parent).expect("remove scoped test directory");
        }

        #[test]
        fn fresh_evidence_root_is_canonical_and_not_created() {
            let run = validate_run_id("booster-scorecard-unit-fresh").expect("valid run");
            let parent = fs::canonicalize(unique_temp_dir("scorecard-fresh"))
                .expect("canonical temp parent");
            let root = parent.join(run.as_str());
            let validated = validate_fresh_evidence_root(&root, &run).expect("fresh root");
            assert_eq!(validated, root);
            assert!(!root.exists());
            fs::remove_dir_all(parent).expect("remove scoped test directory");
        }

        #[test]
        fn manifest_is_deterministic_for_a_small_tree() {
            let root = unique_temp_dir("scorecard-manifest");
            fs::create_dir(root.join("nested")).expect("create nested");
            fs::write(root.join("a"), b"alpha").expect("write a");
            fs::write(root.join("nested").join("b"), b"beta").expect("write b");
            let first = profile_r0(&root).expect("first profile");
            let second = profile_r0(&root).expect("second profile");
            assert_eq!(first, second);
            assert_eq!(first.regular_files, 2);
            assert_eq!(first.directories, 2);
            assert_eq!(first.logical_bytes, 9);
            fs::remove_dir_all(root).expect("remove scoped test directory");
        }

        fn unique_temp_dir(label: &str) -> PathBuf {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()));
            fs::create_dir(&path).expect("create scoped test directory");
            path
        }
    }
}
