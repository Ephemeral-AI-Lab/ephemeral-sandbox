mod security_profile {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/command/service/security_profile.rs"
    ));
}

use sandbox_runtime_mpla_poc::PREPARED_FIXTURE_PROFILE;
use sandbox_runtime_namespace_execution::CommandSecurityProfile;
use security_profile::selected_command_security_profile;

const QUALIFICATION: CommandSecurityProfile = CommandSecurityProfile::MplaBenchmarkQualification;

#[test]
fn only_frozen_benchmark_commands_receive_the_qualification_profile() {
    let authority = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 authority-probe --probe-root /eos/workspace/mpla-poc/authority/run-1";
    let measurement = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 measure --run-id run-1 --run-root /eos/workspace/mpla-poc/speed/run-1 --oracle /eos/layer-stack/base/B000001-base/_campaign-tools/mpla-poc-oracle --catalog-exporter /eos/layer-stack/base/B000001-base/_campaign-tools/sandbox-catalog-export --catalog /eos/layer-stack/base/B000001-base/_campaign-tools/product-catalog.json --build-commit 0123456789abcdef0123456789abcdef01234567 --samples-ledger /eos/workspace/samples.jsonl";
    let qualification = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id mpla-booster-20260729T151100Z --case qualification --candidate-sandbox-id eos-a600e51b-eb32-44d5-af5d-443b5bcc3f40 --build-commit 11f793ad96754e612ca6615835c11d78cf443a83";
    let publication_preparation = format!("/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 prepare-publication-fixture --run-id run-1 --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567 --fixture-profile {PREPARED_FIXTURE_PROFILE}");
    let fixture_publication_preparation = format!("/eos/mpla-fixtures/{PREPARED_FIXTURE_PROFILE}/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 prepare-publication-fixture --run-id run-1 --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567 --fixture-profile {PREPARED_FIXTURE_PROFILE}");
    let fixture_cache_builder = format!("/eos/mpla-fixtures/{PREPARED_FIXTURE_PROFILE}/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 build-publication-fixture-cache --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567");
    let cache_inspection = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 inspect-prepared-fixture-cache";
    let activation = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case activation --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let fork = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case fork --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let rollback = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case rollback --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let squash = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case squash --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let publication = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case publication --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let stream = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case stream --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let recovery = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case recovery --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let fixture_publication = format!("/eos/mpla-fixtures/{PREPARED_FIXTURE_PROFILE}/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case publication --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567");
    let staged_publication = "/workspace/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case publication --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let staged_preparation = format!("/workspace/_campaign-tools/mpla-speed-poc-v1 prepare-publication-fixture --run-id run-1 --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567 --fixture-profile {PREPARED_FIXTURE_PROFILE}");
    let staged_activation_preparation = "/workspace/_campaign-tools/mpla-speed-poc-v1 prepare-lifecycle-control --run-id run-1 --phase activation --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let staged_fork_preparation = "/workspace/_campaign-tools/mpla-speed-poc-v1 prepare-lifecycle-control --run-id run-1 --phase fork --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let staged_rollback_preparation = "/workspace/_campaign-tools/mpla-speed-poc-v1 prepare-lifecycle-control --run-id run-1 --phase rollback --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";

    for command in [
        authority,
        measurement,
        qualification,
        publication_preparation.as_str(),
        fixture_publication_preparation.as_str(),
        fixture_cache_builder.as_str(),
        cache_inspection,
        activation,
        fork,
        rollback,
        squash,
        publication,
        stream,
        recovery,
        fixture_publication.as_str(),
        staged_publication,
        staged_preparation.as_str(),
        staged_activation_preparation,
        staged_fork_preparation,
        staged_rollback_preparation,
    ] {
        assert_eq!(
            selected_command_security_profile(QUALIFICATION, command),
            QUALIFICATION
        );
    }
    assert_eq!(
        selected_command_security_profile(
            QUALIFICATION,
            "mount -t tmpfs none .mpla-ordinary-mount-probe"
        ),
        CommandSecurityProfile::Standard
    );
}

#[test]
fn fixture_builder_mount_path_receives_qualification_profile() {
    let command = format!("/eos/mpla-fixtures/{PREPARED_FIXTURE_PROFILE}/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 build-publication-fixture-cache --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567");

    assert_eq!(
        selected_command_security_profile(QUALIFICATION, &command),
        QUALIFICATION
    );
}

#[test]
fn shell_suffixes_and_contract_drift_are_not_privileged() {
    let suffix = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 authority-probe --probe-root /eos/workspace/mpla-poc/authority/run-1 ; mount -t tmpfs none /mnt";
    let traversal = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 authority-probe --probe-root /eos/workspace/mpla-poc/authority/../escape";
    let wrong_case = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case other --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let retired_lifecycle_case = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case lifecycle --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let extra_scorecard_arg = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case lifecycle --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567 --token secret";
    let preparation_with_case = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 prepare-publication-fixture --run-id run-1 --case publication --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567 --fixture-profile s4-chain-v5";
    let preparation_without_profile = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 prepare-publication-fixture --run-id run-1 --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let normal_root_cache_builder = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 build-publication-fixture-cache --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let staged_cache_builder = "/workspace/_campaign-tools/mpla-speed-poc-v1 build-publication-fixture-cache --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let cache_inspection_with_argument = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 inspect-prepared-fixture-cache --branch fixture-depth-1";
    let staged_authority_probe = "/workspace/_campaign-tools/mpla-speed-poc-v1 authority-probe --probe-root /eos/workspace/mpla-poc/authority/run-1";
    let staged_sibling = "/workspace/_campaign-tools-untrusted/mpla-speed-poc-v1 scorecard-case --run-id run-1 --case publication --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let lifecycle_wrong_phase = "/workspace/_campaign-tools/mpla-speed-poc-v1 prepare-lifecycle-control --run-id run-1 --phase squash --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567";
    let lifecycle_extra_argument = "/workspace/_campaign-tools/mpla-speed-poc-v1 prepare-lifecycle-control --run-id run-1 --phase activation --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567 --token secret";
    let fixture_builder_lifecycle_preparation = format!("/eos/mpla-fixtures/{PREPARED_FIXTURE_PROFILE}/layer-stack/base/B000001-base/_campaign-tools/mpla-speed-poc-v1 prepare-lifecycle-control --run-id run-1 --phase activation --candidate-sandbox-id eos-candidate-1 --build-commit 0123456789abcdef0123456789abcdef01234567");

    for command in [
        suffix,
        traversal,
        wrong_case,
        retired_lifecycle_case,
        extra_scorecard_arg,
        preparation_with_case,
        preparation_without_profile,
        normal_root_cache_builder,
        staged_cache_builder,
        cache_inspection_with_argument,
        staged_authority_probe,
        staged_sibling,
        lifecycle_wrong_phase,
        lifecycle_extra_argument,
        fixture_builder_lifecycle_preparation.as_str(),
    ] {
        assert_eq!(
            selected_command_security_profile(QUALIFICATION, command),
            CommandSecurityProfile::Standard
        );
    }
}
