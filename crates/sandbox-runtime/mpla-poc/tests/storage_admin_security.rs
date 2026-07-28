use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sandbox_runtime_mpla_poc::storage_admin::{
    authorize_storage_admin, decode_invocation, run_platform_invocation, run_storage_admin,
    storage_admin_authorized_path_sha256, storage_admin_mount_attestation_sha256,
    storage_admin_mount_plan_evidence, storage_admin_mountinfo_target_sha256,
    storage_admin_process_evidence_from_status, validate_opened_mount_namespace,
    OrdinaryWorkloadPolicy, StorageAdminCapabilityProfile, StorageAdminExecution,
    StorageAdminInvocation, StorageAdminLifecycle, StorageAdminLowerBinding,
    StorageAdminMountAttestation, StorageAdminMountReceiptBinding, StorageAdminMountTableEvidence,
    StorageAdminObservedMount, StorageAdminPathIdentity, StorageAdminPreparationStep,
    StorageAdminProcessProfile, StorageAdminTargetBinding, STORAGE_ADMIN_SECCOMP_PROFILE_ID,
};
use sandbox_runtime_mpla_poc::{
    AllocationId, OperationId, RunId, SessionId, StorageAdminAction, StorageAdminAuthorization,
    StorageAdminOutcome, StorageAdminRequest, StorageAdminScope, INTERFACE_VERSION, SCHEMA_VERSION,
    STORAGE_ADMIN_EFFECTIVE_CAPABILITIES,
    STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_EFFECTIVE_CAPABILITIES,
    STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID,
    STORAGE_ADMIN_PRIVILEGED_SYSCALLS, STORAGE_ADMIN_PROFILE_ID, STORAGE_ADMIN_TRUSTED_EXECUTABLE,
};
use uuid::Uuid;

const TRUSTED_ACTOR: &str = "mpla-corrective-lead";
const NAMESPACE_HOLDER_PID: u32 = 4_242;

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mpla-storage-admin-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create test root");
        Self(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct FakeLifecycle {
    execution: StorageAdminExecution,
    executions: usize,
    recoveries: usize,
    commits: usize,
    capability_mask: u64,
    mountinfo_after: Option<StorageAdminMountTableEvidence>,
    attestation_mutation: Option<AttestationMutation>,
    input_access_mutation: Option<InputAccessMutation>,
}

#[derive(Clone, Copy)]
enum AttestationMutation {
    LowerPath,
    LowerOrder,
    LowerIdentity,
    LeaseEpoch,
    RequestHash,
    WorkspaceTarget,
    TargetMountId,
    TargetDigest,
    Namespace,
    Filesystem,
    Source,
    MountOptions,
    SuperOptions,
    Upper,
    Work,
    Profile,
    Capabilities,
    MissingAttestation,
    MissingBinding,
    BindingDigest,
    BindingOperation,
}

#[derive(Clone, Copy)]
enum InputAccessMutation {
    Missing,
    RequestedModes,
}

impl FakeLifecycle {
    fn succeeding() -> Self {
        Self {
            execution: StorageAdminExecution::succeeded(),
            executions: 0,
            recoveries: 0,
            commits: 0,
            capability_mask: 1 << 21,
            mountinfo_after: None,
            attestation_mutation: None,
            input_access_mutation: None,
        }
    }

    fn returning(execution: StorageAdminExecution) -> Self {
        Self {
            execution,
            executions: 0,
            recoveries: 0,
            commits: 0,
            capability_mask: 1 << 21,
            mountinfo_after: None,
            attestation_mutation: None,
            input_access_mutation: None,
        }
    }

    fn with_capability_mask(mut self, capability_mask: u64) -> Self {
        self.capability_mask = capability_mask;
        self
    }

    fn with_mountinfo_after(mut self, mountinfo_after: StorageAdminMountTableEvidence) -> Self {
        self.mountinfo_after = Some(mountinfo_after);
        self
    }

    fn with_attestation_mutation(mut self, mutation: AttestationMutation) -> Self {
        self.attestation_mutation = Some(mutation);
        self
    }

    fn with_input_access_mutation(mut self, mutation: InputAccessMutation) -> Self {
        self.input_access_mutation = Some(mutation);
        self
    }
}

impl StorageAdminLifecycle for FakeLifecycle {
    fn execute(
        &mut self,
        _action: StorageAdminAction,
        _scope: &StorageAdminScope,
    ) -> StorageAdminExecution {
        self.executions += 1;
        self.execution.clone()
    }

    fn recover_incomplete(
        &mut self,
        _action: StorageAdminAction,
        _scope: &StorageAdminScope,
    ) -> StorageAdminExecution {
        self.recoveries += 1;
        StorageAdminExecution::failed("recovered incomplete test operation", true)
    }

    fn receipt_committed(&mut self, _action: StorageAdminAction, _scope: &StorageAdminScope) {
        self.commits += 1;
    }

    fn authority_evidence(
        &mut self,
        scope: &StorageAdminScope,
    ) -> sandbox_runtime_mpla_poc::PocResult<(
        sandbox_runtime_mpla_poc::storage_admin::StorageAdminProcessEvidence,
        sandbox_runtime_mpla_poc::storage_admin::StorageAdminMountPlanEvidence,
    )> {
        let status = format!(
            "CapInh:\t0000000000000000\nCapPrm:\t{mask:016x}\nCapEff:\t{mask:016x}\nCapBnd:\t{mask:016x}\nCapAmb:\t0000000000000000\nNoNewPrivs:\t1\nSeccomp:\t2\nSeccomp_filters:\t1\n",
            mask = self.capability_mask,
        );
        let mut mount_plan = storage_admin_mount_plan_evidence(scope)?;
        match self.input_access_mutation {
            Some(InputAccessMutation::Missing) => mount_plan.input_access.paths.clear(),
            Some(InputAccessMutation::RequestedModes) => {
                mount_plan.input_access.paths[0].effective_access[0].requested =
                    vec!["write".to_owned()];
            }
            None => {}
        }
        if let Some(mountinfo_after) = self.mountinfo_after.clone() {
            mount_plan.mountinfo_after = mountinfo_after;
        } else {
            mount_plan.mountinfo_after = observed_workspace_mount(scope, mount_plan.source.clone());
        }
        Ok((
            storage_admin_process_evidence_from_status(
                PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
                "00".repeat(32),
                &status,
                PathBuf::from("/mpla-test/cgroup.procs"),
                NAMESPACE_HOLDER_PID,
                scope.mount_namespace_id.clone(),
                scope
                    .mount_namespace_id
                    .trim_start_matches("mnt:[")
                    .trim_end_matches(']')
                    .parse()
                    .expect("valid mount namespace inode"),
            )?,
            mount_plan,
        ))
    }

    fn mount_authority_evidence(
        &mut self,
        selection: &sandbox_runtime_mpla_poc::storage_admin::StorageAdminSelection,
        process: &sandbox_runtime_mpla_poc::storage_admin::StorageAdminProcessEvidence,
        mount_plan: &sandbox_runtime_mpla_poc::storage_admin::StorageAdminMountPlanEvidence,
    ) -> sandbox_runtime_mpla_poc::PocResult<(
        Option<StorageAdminMountAttestation>,
        Option<StorageAdminMountReceiptBinding>,
    )> {
        if selection.request().action != StorageAdminAction::Mount {
            return Ok((None, None));
        }
        let observed = mount_plan
            .mountinfo_after
            .target
            .as_ref()
            .expect("successful fake mount has a target");
        let identity = StorageAdminPathIdentity {
            mount_id: observed.mount_id,
            device_major: 8,
            device_minor: 1,
            inode: 9001,
        };
        let mut attestation = StorageAdminMountAttestation {
            schema_version: SCHEMA_VERSION,
            run_id: selection.request().scope.run_id.clone(),
            sandbox_id: selection.request().scope.sandbox_id.clone(),
            workspace_session_id: selection.request().scope.workspace_session_id.clone(),
            session_id: selection.request().scope.session_id.clone(),
            allocation_id: selection.request().scope.allocation_id.clone(),
            lease_id: selection.request().scope.lease_id.clone(),
            lease_epoch: selection.request().scope.lease_epoch,
            mount_namespace_id: selection.request().scope.mount_namespace_id.clone(),
            mount_namespace_inode: process.mount_namespace_inode,
            storage_operation_id: selection.request().operation_id.clone(),
            request_sha256: selection.request_sha256().to_owned(),
            lower_bindings_newest_first: selection
                .request()
                .scope
                .lower_dirs_newest_first
                .iter()
                .enumerate()
                .map(|(index, path)| StorageAdminLowerBinding {
                    index,
                    authorized_path_sha256: storage_admin_authorized_path_sha256(path),
                    fd_identity: identity.clone(),
                    authorized_path_identity: identity.clone(),
                })
                .collect(),
            target: StorageAdminTargetBinding {
                workspace_target: selection.request().scope.workspace_root.clone(),
                mount_namespace_id: selection.request().scope.mount_namespace_id.clone(),
                mount_namespace_inode: process.mount_namespace_inode,
                mount_id: observed.mount_id,
                mountinfo_sha256: mount_plan.mountinfo_after.sha256.clone(),
                target_identity: identity,
                filesystem_type: observed.filesystem_type.clone(),
                source: observed.source.clone(),
                mount_options: observed.mount_options.clone(),
                super_options: observed.super_options.clone(),
                expected_upperdir_sha256: storage_admin_authorized_path_sha256(
                    &mount_plan.upper_dir,
                ),
                observed_upperdir_sha256: storage_admin_authorized_path_sha256(
                    observed.upper_dir.as_deref().expect("fake upperdir"),
                ),
                expected_workdir_sha256: storage_admin_authorized_path_sha256(&mount_plan.work_dir),
                observed_workdir_sha256: storage_admin_authorized_path_sha256(
                    observed.work_dir.as_deref().expect("fake workdir"),
                ),
            },
            profile_id: selection.profile_id().to_owned(),
            effective_capabilities: selection
                .profile()
                .effective_capabilities()
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        };
        match self.attestation_mutation {
            Some(AttestationMutation::LowerPath) => {
                attestation.lower_bindings_newest_first[0].authorized_path_sha256 = "ff".repeat(32);
            }
            Some(AttestationMutation::LowerOrder) => {
                attestation.lower_bindings_newest_first.swap(0, 1);
            }
            Some(AttestationMutation::LowerIdentity) => {
                attestation.lower_bindings_newest_first[0]
                    .authorized_path_identity
                    .inode += 1;
            }
            Some(AttestationMutation::LeaseEpoch) => {
                attestation.lease_epoch += 1;
            }
            Some(AttestationMutation::RequestHash) => {
                attestation.request_sha256 = "ff".repeat(32);
            }
            Some(AttestationMutation::WorkspaceTarget) => {
                attestation.target.workspace_target = PathBuf::from("/forged/workspace");
            }
            Some(AttestationMutation::TargetMountId) => {
                attestation.target.mount_id += 1;
            }
            Some(AttestationMutation::TargetDigest) => {
                attestation.target.mountinfo_sha256 = "ff".repeat(32);
            }
            Some(AttestationMutation::Namespace) => {
                attestation.target.mount_namespace_id = "mnt:[1]".to_owned();
            }
            Some(AttestationMutation::Filesystem) => {
                attestation.target.filesystem_type = "forged".to_owned();
            }
            Some(AttestationMutation::Source) => {
                attestation.target.source = "forged".to_owned();
            }
            Some(AttestationMutation::MountOptions) => {
                attestation
                    .target
                    .mount_options
                    .retain(|option| option != "nodev");
            }
            Some(AttestationMutation::SuperOptions) => {
                attestation.target.super_options.push("forged".to_owned());
            }
            Some(AttestationMutation::Upper) => {
                attestation.target.observed_upperdir_sha256 = "ff".repeat(32);
            }
            Some(AttestationMutation::Work) => {
                attestation.target.observed_workdir_sha256 = "ff".repeat(32);
            }
            Some(AttestationMutation::Profile) => {
                attestation.profile_id = "forged-profile".to_owned();
            }
            Some(AttestationMutation::Capabilities) => {
                attestation
                    .effective_capabilities
                    .push("CAP_SYS_PTRACE".to_owned());
            }
            Some(AttestationMutation::MissingAttestation) => {
                return Ok((None, None));
            }
            Some(AttestationMutation::MissingBinding) => {
                return Ok((Some(attestation), None));
            }
            Some(AttestationMutation::BindingDigest)
            | Some(AttestationMutation::BindingOperation)
            | None => {}
        }
        let mut binding = StorageAdminMountReceiptBinding {
            storage_operation_id: selection.request().operation_id.clone(),
            attestation_sha256: storage_admin_mount_attestation_sha256(&attestation)?,
        };
        match self.attestation_mutation {
            Some(AttestationMutation::BindingDigest) => {
                binding.attestation_sha256 = "ff".repeat(32);
            }
            Some(AttestationMutation::BindingOperation) => {
                binding.storage_operation_id = OperationId::from_string("forged-operation");
            }
            _ => {}
        }
        Ok((Some(attestation), Some(binding)))
    }
}

fn invocation(root: &Path, operation_id: &str) -> StorageAdminInvocation {
    let scope = StorageAdminScope {
        run_id: RunId::parse("m2r-20260728T015724p0800").expect("run id"),
        sandbox_id: "sandbox-security".to_owned(),
        workspace_session_id: "workspace-security".to_owned(),
        session_id: SessionId::from_string("session-security"),
        allocation_id: AllocationId::from_string("allocation-security"),
        lease_id: "m2r-lease-security:7".to_owned(),
        lease_epoch: 7,
        mount_namespace_id: "mnt:[4026532999]".to_owned(),
        payload_root: root.join("payload"),
        control_root: root.join("control"),
        lower_dirs_newest_first: vec![root.join("payload/lower-1")],
        allocation_root: root.join("allocation"),
        workspace_root: root.join("workspace"),
    };
    let request = StorageAdminRequest {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        profile_id: STORAGE_ADMIN_PROFILE_ID.to_owned(),
        operation_id: OperationId::from_string(operation_id),
        action: StorageAdminAction::Mount,
        scope: scope.clone(),
    };
    let authorization = StorageAdminAuthorization {
        authenticated: true,
        actor_id: TRUSTED_ACTOR.to_owned(),
        operation_id: request.operation_id.clone(),
        run_id: scope.run_id.clone(),
        sandbox_id: scope.sandbox_id.clone(),
        workspace_session_id: scope.workspace_session_id.clone(),
        session_id: scope.session_id.clone(),
        allocation_id: scope.allocation_id.clone(),
        lease_id: scope.lease_id.clone(),
        lease_epoch: scope.lease_epoch,
        mount_namespace_id: scope.mount_namespace_id.clone(),
    };
    StorageAdminInvocation {
        expected_request: request.clone(),
        request,
        authorization,
        trusted_actor_id: TRUSTED_ACTOR.to_owned(),
        trusted_executable_sha256: "00".repeat(32),
        workload_cgroup_procs: root.join("workload/cgroup.procs"),
        mount_namespace_holder_pid: NAMESPACE_HOLDER_PID,
        mount_receipt_binding: None,
    }
}

fn rejection(invocation: &StorageAdminInvocation) -> String {
    authorize_storage_admin(
        &invocation.expected_request,
        &invocation.request,
        &invocation.authorization,
        &invocation.trusted_actor_id,
    )
    .expect_err("substitution must fail closed")
    .to_string()
}

fn observed_workspace_mount(
    scope: &StorageAdminScope,
    source: String,
) -> StorageAdminMountTableEvidence {
    let mut mount_options = vec![
        "rw".to_owned(),
        "nosuid".to_owned(),
        "nodev".to_owned(),
        "relatime".to_owned(),
    ];
    mount_options.sort();
    let target = StorageAdminObservedMount {
        mount_id: 47,
        parent_mount_id: 1,
        root: PathBuf::from("/"),
        source,
        filesystem_type: "overlay".to_owned(),
        target: scope.workspace_root.clone(),
        mount_options,
        optional_fields: Vec::new(),
        super_options: vec!["rw".to_owned()],
        upper_dir: Some(scope.allocation_root.join("upper")),
        work_dir: Some(scope.allocation_root.join("work")),
    };
    StorageAdminMountTableEvidence {
        sha256: storage_admin_mountinfo_target_sha256(Some(&target))
            .expect("hash fake target mount"),
        target: Some(target),
    }
}

#[test]
fn only_exact_authenticated_lifecycle_request_selects_storage_profile() {
    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-exact");
    let selection = authorize_storage_admin(
        &invocation.expected_request,
        &invocation.request,
        &invocation.authorization,
        &invocation.trusted_actor_id,
    )
    .expect("exact request selects profile");

    assert_eq!(selection.request(), &invocation.request);
    assert_eq!(selection.profile_id(), STORAGE_ADMIN_PROFILE_ID);
    assert_eq!(
        selection.trusted_executable(),
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    );
    assert_eq!(selection.request_sha256().len(), 64);

    let mut unauthenticated = invocation.clone();
    unauthenticated.authorization.authenticated = false;
    assert!(rejection(&unauthenticated).contains("not authenticated"));

    let mut wrong_actor = invocation.clone();
    wrong_actor.authorization.actor_id = "ordinary-workload".to_owned();
    assert!(rejection(&wrong_actor).contains("actor id"));
}

#[test]
fn caller_cannot_escalate_a_production_request_to_the_qualification_profile() {
    let root = TestRoot::new();
    let mut untrusted = invocation(&root.0, "operation-profile-escalation");
    untrusted.request.profile_id =
        STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID.to_owned();

    assert!(rejection(&untrusted).contains("profile id"));
    assert_eq!(
        StorageAdminCapabilityProfile::Production.profile_id(),
        STORAGE_ADMIN_PROFILE_ID
    );
    assert_eq!(
        StorageAdminCapabilityProfile::OverlayfsDacOverrideQualification.profile_id(),
        STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID
    );
}

#[test]
fn malformed_mount_binding_is_rejected_before_platform_process_preparation() {
    let root = TestRoot::new();
    let mut invocation = invocation(&root.0, "operation-early-binding-validation");
    invocation.mount_receipt_binding = Some(StorageAdminMountReceiptBinding {
        storage_operation_id: OperationId::from_string("prior-mount"),
        attestation_sha256: "00".repeat(32),
    });

    let error = run_platform_invocation(&invocation).expect_err("mount binding must be rejected");
    assert!(error
        .to_string()
        .contains("cannot supply prior mount authority"));
}

#[test]
fn qualification_profile_receipt_records_only_the_targeted_extra_capability() {
    let root = TestRoot::new();
    let mut invocation = invocation(&root.0, "operation-qualification-receipt");
    invocation.expected_request.profile_id =
        STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID.to_owned();
    invocation.request.profile_id =
        STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID.to_owned();
    let mut lifecycle = FakeLifecycle::succeeding().with_capability_mask((1 << 21) | (1 << 1));

    let receipt = run_storage_admin(&invocation, &mut lifecycle).expect("qualification receipt");
    assert_eq!(
        receipt.profile_id,
        STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID
    );
    assert_eq!(
        receipt.effective_capabilities,
        STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_EFFECTIVE_CAPABILITIES
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        receipt.process_evidence.capabilities.effective,
        (1 << 21) | (1 << 1)
    );
    assert_eq!(
        receipt.process_evidence.capabilities.permitted,
        (1 << 21) | (1 << 1)
    );
    assert_eq!(
        receipt.process_evidence.capabilities.bounding,
        (1 << 21) | (1 << 1)
    );
}

#[test]
fn mount_input_access_evidence_must_exactly_cover_the_bound_plan() {
    for (operation_id, mutation, expected) in [
        (
            "operation-input-access-missing",
            InputAccessMutation::Missing,
            "does not cover",
        ),
        (
            "operation-input-access-modes",
            InputAccessMutation::RequestedModes,
            "requested modes",
        ),
    ] {
        let root = TestRoot::new();
        let invocation = invocation(&root.0, operation_id);
        let mut lifecycle = FakeLifecycle::succeeding().with_input_access_mutation(mutation);

        let error =
            run_storage_admin(&invocation, &mut lifecycle).expect_err("evidence must be rejected");
        assert!(error.to_string().contains(expected));
        assert_eq!(lifecycle.commits, 0);
    }
}

#[test]
fn measured_raw_mount_api_representation_is_accepted_only_when_all_trusted_fields_match() {
    let root = TestRoot::new();
    let valid = invocation(&root.0, "operation-measured-mountinfo-valid");
    let plan = storage_admin_mount_plan_evidence(&valid.request.scope).expect("mount plan");
    let valid_observed = observed_workspace_mount(&valid.request.scope, plan.source.clone());
    let mut valid_lifecycle = FakeLifecycle::succeeding().with_mountinfo_after(valid_observed);
    run_storage_admin(&valid, &mut valid_lifecycle).expect("matching observed mount is accepted");
    assert_eq!(valid_lifecycle.commits, 1);

    let mut forged_source = invocation(&root.0, "operation-measured-mountinfo-forged-source");
    let mut forged_observed =
        observed_workspace_mount(&forged_source.request.scope, "forged".to_owned());
    let mut forged_lifecycle =
        FakeLifecycle::succeeding().with_mountinfo_after(forged_observed.clone());
    assert!(run_storage_admin(&forged_source, &mut forged_lifecycle).is_err());
    assert_eq!(forged_lifecycle.commits, 0);

    for label in ["filesystem", "options", "upper", "work"] {
        let operation_id = format!("operation-measured-mountinfo-{label}");
        forged_source = invocation(&root.0, &operation_id);
        forged_observed =
            observed_workspace_mount(&forged_source.request.scope, plan.source.clone());
        let entry = forged_observed
            .target
            .as_mut()
            .expect("workspace target entry");
        match label {
            "filesystem" => entry.filesystem_type = "forged".to_owned(),
            "options" => entry.mount_options.retain(|option| option != "nosuid"),
            "upper" => entry.upper_dir = Some(PathBuf::from("/forged/upper")),
            "work" => entry.work_dir = Some(PathBuf::from("/forged/work")),
            _ => unreachable!("fixed mismatch case"),
        }
        forged_lifecycle = FakeLifecycle::succeeding().with_mountinfo_after(forged_observed);
        assert!(
            run_storage_admin(&forged_source, &mut forged_lifecycle).is_err(),
            "{label} mismatch must fail closed"
        );
        assert_eq!(forged_lifecycle.commits, 0, "{label} mismatch committed");
    }
}

#[test]
fn durable_mount_attestation_rejects_every_identity_substitution_class() {
    let root = TestRoot::new();
    let cases = [
        ("lower-path", AttestationMutation::LowerPath),
        ("lower-order", AttestationMutation::LowerOrder),
        ("lower-identity", AttestationMutation::LowerIdentity),
        ("lease-epoch", AttestationMutation::LeaseEpoch),
        ("request-hash", AttestationMutation::RequestHash),
        ("workspace-target", AttestationMutation::WorkspaceTarget),
        ("target-mount-id", AttestationMutation::TargetMountId),
        ("target-digest", AttestationMutation::TargetDigest),
        ("namespace", AttestationMutation::Namespace),
        ("filesystem", AttestationMutation::Filesystem),
        ("source", AttestationMutation::Source),
        ("mount-options", AttestationMutation::MountOptions),
        ("super-options", AttestationMutation::SuperOptions),
        ("upper", AttestationMutation::Upper),
        ("work", AttestationMutation::Work),
        ("profile", AttestationMutation::Profile),
        ("capabilities", AttestationMutation::Capabilities),
        (
            "missing-attestation",
            AttestationMutation::MissingAttestation,
        ),
        ("missing-binding", AttestationMutation::MissingBinding),
        ("binding-digest", AttestationMutation::BindingDigest),
        ("binding-operation", AttestationMutation::BindingOperation),
    ];
    for (label, mutation) in cases {
        let mut invocation = invocation(&root.0, &format!("operation-attestation-{label}"));
        invocation
            .request
            .scope
            .lower_dirs_newest_first
            .push(root.0.join(format!("payload/{label}-second-lower")));
        invocation.expected_request = invocation.request.clone();
        let mut lifecycle = FakeLifecycle::succeeding().with_attestation_mutation(mutation);
        let error = run_storage_admin(&invocation, &mut lifecycle)
            .expect_err("forged attestation must fail closed")
            .to_string();
        assert!(
            error.contains("attestation")
                || error.contains("opened lower")
                || error.contains("mount receipt"),
            "{label} returned an unexpected rejection: {error}"
        );
        assert_eq!(lifecycle.commits, 0, "{label} forged receipt committed");
    }
}

#[test]
fn failed_mount_receipt_retains_only_the_bounded_target_diagnostic() {
    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-receipt-diagnostic");
    let observed =
        observed_workspace_mount(&invocation.request.scope, "raw-mount-api-source".to_owned());
    let mut lifecycle = FakeLifecycle::succeeding().with_mountinfo_after(observed);

    let error = run_storage_admin(&invocation, &mut lifecycle)
        .expect_err("mismatched source must reject the receipt")
        .to_string();
    assert!(error.contains("receipt observed mount source does not match trusted binding"));
    assert!(error.contains("raw-mount-api-source"));
    assert_eq!(lifecycle.commits, 0);

    let diagnostic_path = root.0.join(
        "control/storage-admin/operation-receipt-diagnostic/RECEIPT_VALIDATION_DIAGNOSTIC.json",
    );
    let diagnostic: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&diagnostic_path).expect("read immutable receipt diagnostic"),
    )
    .expect("decode receipt diagnostic");
    let fields: BTreeSet<_> = diagnostic
        .as_object()
        .expect("diagnostic object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        fields,
        [
            "schema_version",
            "interface_version",
            "operation_id",
            "request_sha256",
            "mountinfo_sha256",
            "filesystem_type",
            "parsed_source",
            "mount_options",
            "trusted_expected_source",
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        diagnostic["mountinfo_sha256"],
        storage_admin_mountinfo_target_sha256(Some(
            &observed_workspace_mount(&invocation.request.scope, "raw-mount-api-source".to_owned())
                .target
                .expect("observed target")
        ))
        .expect("hash observed target")
    );
    assert_eq!(diagnostic["filesystem_type"], "overlay");
    assert_eq!(diagnostic["parsed_source"], "raw-mount-api-source");
    assert_eq!(
        diagnostic["mount_options"],
        serde_json::json!(["nodev", "nosuid", "relatime", "rw"])
    );
    assert_eq!(diagnostic["trusted_expected_source"], "none");
    assert!(diagnostic.get("target").is_none());
    assert!(diagnostic.get("lower_dirs_newest_first").is_none());
}

#[test]
fn every_request_scope_substitution_is_rejected_before_execution() {
    let root = TestRoot::new();
    let original = invocation(&root.0, "operation-binding");
    let mut substitutions = Vec::new();

    let mut changed = original.clone();
    changed.request.operation_id = OperationId::from_string("operation-other");
    substitutions.push(("operation", changed));

    let mut changed = original.clone();
    changed.request.scope.run_id = RunId::parse("m2r-other").expect("run id");
    substitutions.push(("run", changed));

    let mut changed = original.clone();
    changed.request.scope.sandbox_id = "sandbox-other".to_owned();
    substitutions.push(("sandbox", changed));

    let mut changed = original.clone();
    changed.request.scope.workspace_session_id = "workspace-other".to_owned();
    substitutions.push(("workspace", changed));

    let mut changed = original.clone();
    changed.request.scope.session_id = SessionId::from_string("session-other");
    substitutions.push(("MPLA session", changed));

    let mut changed = original.clone();
    changed.request.scope.allocation_id = AllocationId::from_string("allocation-other");
    substitutions.push(("allocation", changed));

    let mut changed = original.clone();
    changed.request.scope.lease_id = "m2r-lease-other:8".to_owned();
    substitutions.push(("lease id", changed));

    let mut changed = original.clone();
    changed.request.scope.lease_epoch += 1;
    substitutions.push(("lease epoch", changed));

    let mut changed = original.clone();
    changed.request.scope.mount_namespace_id = "mnt:[4026533000]".to_owned();
    substitutions.push(("mount namespace", changed));

    let mut changed = original.clone();
    changed.request.scope.payload_root = root.0.join("payload-other");
    substitutions.push(("payload root", changed));

    let mut changed = original.clone();
    changed.request.scope.control_root = root.0.join("control-other");
    substitutions.push(("control root", changed));

    let mut changed = original.clone();
    changed.request.scope.lower_dirs_newest_first = vec![root.0.join("payload/lower-other")];
    substitutions.push(("lower directories", changed));

    let mut changed = original.clone();
    changed.request.scope.allocation_root = root.0.join("allocation-other");
    substitutions.push(("allocation root", changed));

    let mut changed = original.clone();
    changed.request.scope.workspace_root = root.0.join("workspace-other");
    substitutions.push(("workspace root", changed));

    let mut changed = original.clone();
    changed.request.action = StorageAdminAction::Cleanup;
    substitutions.push(("lifecycle action", changed));

    for (label, substituted) in substitutions {
        assert!(
            rejection(&substituted).contains(label),
            "{label} substitution did not identify its rejected binding"
        );
    }

    let mut lifecycle = FakeLifecycle::succeeding();
    let mut substituted = original.clone();
    substituted.request.scope.workspace_root = root.0.join("attacker-workspace");
    assert!(run_storage_admin(&substituted, &mut lifecycle).is_err());
    assert_eq!(lifecycle.executions, 0);
    assert_eq!(lifecycle.commits, 0);
}

#[test]
fn executable_command_shell_capability_and_extra_path_inputs_are_not_accepted() {
    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-wire");

    for (field, injected_value) in [
        (
            "executable",
            serde_json::json!("/tmp/attacker-storage-admin"),
        ),
        ("command", serde_json::json!(["/bin/sh", "-c", "mount"])),
        (
            "capabilities",
            serde_json::json!(["CAP_SYS_ADMIN", "CAP_SYS_PTRACE"]),
        ),
        ("namespace_path", serde_json::json!("/proc/1/ns/mnt")),
        ("allowed_path", serde_json::json!("/")),
    ] {
        let mut encoded = serde_json::to_value(&invocation).expect("encode invocation");
        encoded["request"]
            .as_object_mut()
            .expect("request object")
            .insert(field.to_owned(), injected_value);
        let bytes = serde_json::to_vec(&encoded).expect("encode substituted invocation");
        assert!(
            decode_invocation(&bytes).is_err(),
            "{field} must not be an accepted wire field"
        );
    }

    let profile = StorageAdminProcessProfile;
    assert_eq!(
        profile.trusted_executable(),
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    );
    assert!(!profile.allows_arbitrary_executable());

    let mut encoded = serde_json::to_value(&invocation).expect("encode invocation");
    encoded.as_object_mut().expect("invocation object").insert(
        "mount_namespace_path".to_owned(),
        serde_json::json!("/proc/1/ns/mnt"),
    );
    assert!(decode_invocation(
        &serde_json::to_vec(&encoded).expect("encode path-substituted invocation")
    )
    .is_err());
}

#[test]
fn exact_server_bound_namespace_holder_is_validated_before_setns_and_reverified() {
    let profile = StorageAdminProcessProfile;
    assert_eq!(
        profile
            .user_namespace_path(NAMESPACE_HOLDER_PID)
            .expect("trusted holder path"),
        PathBuf::from("/proc/4242/ns/user")
    );
    assert_eq!(
        profile
            .mount_namespace_path(NAMESPACE_HOLDER_PID)
            .expect("trusted holder path"),
        PathBuf::from("/proc/4242/ns/mnt")
    );
    assert!(profile.mount_namespace_path(0).is_err());

    validate_opened_mount_namespace("mnt:[4026532999]", "mnt:[4026532999]", 4_026_532_999)
        .expect("exact opened namespace");
    assert!(
        validate_opened_mount_namespace("mnt:[4026532999]", "mnt:[4026533000]", 4_026_533_000,)
            .is_err()
    );
    assert!(
        validate_opened_mount_namespace("mnt:[4026532999]", "mnt:[4026532999]", 4_026_533_000,)
            .is_err()
    );

    assert_eq!(
        profile.preparation_steps(),
        &[
            StorageAdminPreparationStep::OpenAndValidateBoundUserNamespace,
            StorageAdminPreparationStep::OpenAndValidateBoundMountNamespace,
            StorageAdminPreparationStep::EnterBoundUserNamespace,
            StorageAdminPreparationStep::VerifyEnteredUserNamespace,
            StorageAdminPreparationStep::EnterBoundMountNamespace,
            StorageAdminPreparationStep::VerifyEnteredMountNamespace,
            StorageAdminPreparationStep::NarrowCapabilityMasks,
            StorageAdminPreparationStep::SetNoNewPrivileges,
            StorageAdminPreparationStep::VerifyExecutableAndCapabilityIdentity,
        ]
    );

    let root = TestRoot::new();
    let mut invalid_holder = invocation(&root.0, "operation-invalid-holder");
    invalid_holder.mount_namespace_holder_pid = 0;
    let mut lifecycle = FakeLifecycle::succeeding();
    assert!(run_storage_admin(&invalid_holder, &mut lifecycle).is_err());
    assert_eq!(lifecycle.executions, 0);
}

#[test]
fn stale_and_fenced_authorizations_fail_closed() {
    let root = TestRoot::new();
    let original = invocation(&root.0, "operation-fence");

    let mut stale_epoch = original.clone();
    stale_epoch.authorization.lease_epoch -= 1;
    assert!(rejection(&stale_epoch).contains("lease epoch"));

    let mut stale_lease = original.clone();
    stale_lease.authorization.lease_id = "expired-lease:6".to_owned();
    assert!(rejection(&stale_lease).contains("lease id"));

    let mut fenced_session = original.clone();
    fenced_session.authorization.workspace_session_id = "fenced-workspace".to_owned();
    assert!(rejection(&fenced_session).contains("workspace session"));

    let mut wrong_namespace = original.clone();
    wrong_namespace.authorization.mount_namespace_id = "mnt:[1]".to_owned();
    assert!(rejection(&wrong_namespace).contains("mount namespace"));

    let mut replayed_for_other_operation = original.clone();
    replayed_for_other_operation.authorization.operation_id =
        OperationId::from_string("operation-prior");
    assert!(rejection(&replayed_for_other_operation).contains("operation id"));
}

#[test]
fn response_loss_retry_returns_one_stable_operation_and_receipt() {
    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-response-loss");
    let mut lifecycle = FakeLifecycle::succeeding();

    let first =
        run_storage_admin(&invocation, &mut lifecycle).expect("first execution must succeed");
    let durable_before = std::fs::read(&first.receipt_path).expect("read durable receipt");
    let durable_text = std::str::from_utf8(&durable_before).expect("receipt is UTF-8 JSON");
    assert!(
        !durable_text.contains("/proc/self/fd/"),
        "a transient FD spelling must never become durable authority"
    );
    assert_eq!(lifecycle.executions, 1);
    assert_eq!(lifecycle.commits, 1);
    assert!(!first.idempotent_replay);

    let mut must_not_execute =
        FakeLifecycle::returning(StorageAdminExecution::failed("must not execute", false));
    let replay =
        run_storage_admin(&invocation, &mut must_not_execute).expect("retry returns receipt");
    let durable_after = std::fs::read(&replay.receipt_path).expect("reread durable receipt");

    assert_eq!(must_not_execute.executions, 0);
    assert_eq!(must_not_execute.recoveries, 0);
    assert_eq!(must_not_execute.commits, 0);
    assert!(replay.idempotent_replay);
    assert_eq!(replay.receipt_path, first.receipt_path);
    assert_eq!(replay.request_sha256, first.request_sha256);
    assert_eq!(replay.outcome, first.outcome);
    assert_eq!(replay.started_unix_ms, first.started_unix_ms);
    assert_eq!(replay.completed_unix_ms, first.completed_unix_ms);
    assert_eq!(durable_after, durable_before);

    let mut substituted_holder = invocation.clone();
    substituted_holder.mount_namespace_holder_pid += 1;
    let mut must_not_replay = FakeLifecycle::succeeding();
    assert!(run_storage_admin(&substituted_holder, &mut must_not_replay).is_err());
    assert_eq!(must_not_replay.executions, 0);
    assert_eq!(must_not_replay.recoveries, 0);
}

#[test]
fn receipt_loss_and_operation_id_collision_fail_closed_without_reexecution() {
    let root = TestRoot::new();
    let original = invocation(&root.0, "operation-lost-receipt");
    let mut lifecycle = FakeLifecycle::succeeding();
    let receipt = run_storage_admin(&original, &mut lifecycle).expect("initial mount receipt");
    std::fs::remove_file(&receipt.receipt_path).expect("simulate lost receipt");

    let mut recovery = FakeLifecycle::succeeding();
    let recovered =
        run_storage_admin(&original, &mut recovery).expect("durable failed recovery receipt");
    assert_eq!(recovery.executions, 0);
    assert_eq!(recovery.recoveries, 1);
    assert_eq!(recovered.outcome, StorageAdminOutcome::Failed);

    let collision_original = invocation(&root.0, "operation-id-collision");
    let mut collision_lifecycle = FakeLifecycle::succeeding();
    run_storage_admin(&collision_original, &mut collision_lifecycle)
        .expect("establish immutable operation");
    let mut collision = collision_original.clone();
    collision.request.scope.workspace_root = root.0.join("other-workspace");
    collision.expected_request = collision.request.clone();
    let mut must_not_execute = FakeLifecycle::succeeding();
    assert!(run_storage_admin(&collision, &mut must_not_execute).is_err());
    assert_eq!(must_not_execute.executions, 0);
    assert_eq!(must_not_execute.recoveries, 0);
}

#[test]
fn failed_and_cancelled_operations_retain_durable_cleanup_evidence() {
    let root = TestRoot::new();
    let failed_invocation = invocation(&root.0, "operation-failed");
    let mut failed_lifecycle =
        FakeLifecycle::returning(StorageAdminExecution::failed("mount rejected", true));
    let failed = run_storage_admin(&failed_invocation, &mut failed_lifecycle)
        .expect("failed operation still returns durable receipt");
    let stored_failed: sandbox_runtime_mpla_poc::StorageAdminReceipt =
        serde_json::from_slice(&std::fs::read(&failed.receipt_path).expect("failed receipt"))
            .expect("decode failed receipt");
    assert_eq!(stored_failed.outcome, StorageAdminOutcome::Failed);
    assert_eq!(stored_failed.failure.as_deref(), Some("mount rejected"));
    assert!(stored_failed.cleanup_complete);

    let cancelled_invocation = invocation(&root.0, "operation-cancelled");
    let mut cancelled_lifecycle =
        FakeLifecycle::returning(StorageAdminExecution::cancelled("lease cancelled", false));
    let cancelled = run_storage_admin(&cancelled_invocation, &mut cancelled_lifecycle)
        .expect("cancelled operation still returns durable receipt");
    let stored_cancelled: sandbox_runtime_mpla_poc::StorageAdminReceipt =
        serde_json::from_slice(&std::fs::read(&cancelled.receipt_path).expect("cancelled receipt"))
            .expect("decode cancelled receipt");
    assert_eq!(stored_cancelled.outcome, StorageAdminOutcome::Cancelled);
    assert_eq!(stored_cancelled.failure.as_deref(), Some("lease cancelled"));
    assert!(!stored_cancelled.cleanup_complete);
}

#[test]
fn ordinary_command_and_workload_policy_has_no_mount_authority() {
    let ordinary = OrdinaryWorkloadPolicy;
    assert!(ordinary.effective_capabilities().is_empty());
    assert!(ordinary.allowed_privileged_syscalls().is_empty());
    assert!(ordinary.denies_syscall("mount"));
    assert!(ordinary.denies_syscall("umount2"));
    assert!(!ordinary.can_select_storage_admin_profile());

    let privileged = StorageAdminProcessProfile;
    assert_eq!(
        privileged.effective_capabilities(),
        STORAGE_ADMIN_EFFECTIVE_CAPABILITIES
    );
    assert_eq!(
        privileged.allowed_privileged_syscalls(),
        STORAGE_ADMIN_PRIVILEGED_SYSCALLS
    );
}

#[test]
fn helper_enters_the_bound_namespaces_before_narrowing_inherited_capabilities() {
    const CAP_NET_ADMIN_BIT: u64 = 1 << 12;
    const CAP_SYS_ADMIN_BIT: u64 = 1 << 21;

    let profile = StorageAdminProcessProfile;
    let daemon_inherited = CAP_SYS_ADMIN_BIT | CAP_NET_ADMIN_BIT;
    assert_ne!(daemon_inherited & CAP_NET_ADMIN_BIT, 0);
    assert_eq!(profile.effective_capability_mask(), CAP_SYS_ADMIN_BIT);
    assert_eq!(profile.permitted_capability_mask(), CAP_SYS_ADMIN_BIT);
    assert_eq!(profile.inheritable_capability_mask(), 0);
    assert_eq!(profile.ambient_capability_mask(), 0);
    assert_eq!(profile.effective_capability_mask() & CAP_NET_ADMIN_BIT, 0);
    assert_eq!(profile.permitted_capability_mask() & CAP_NET_ADMIN_BIT, 0);
    assert_eq!(
        profile.preparation_steps().get(6),
        Some(&StorageAdminPreparationStep::NarrowCapabilityMasks)
    );
    assert_eq!(
        profile.preparation_steps().get(7),
        Some(&StorageAdminPreparationStep::SetNoNewPrivileges)
    );
    assert_eq!(
        profile.preparation_steps().get(8),
        Some(&StorageAdminPreparationStep::VerifyExecutableAndCapabilityIdentity)
    );
}

#[test]
fn internal_security_evidence_captures_raw_process_and_exact_mount_plan() {
    let status = "\
CapInh:\t0000000000000000
CapPrm:\t0000000000200000
CapEff:\t0000000000200000
CapBnd:\t0000000000201000
CapAmb:\t0000000000000000
NoNewPrivs:\t1
Seccomp:\t2
Seccomp_filters:\t1
";
    let process = storage_admin_process_evidence_from_status(
        PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
        "00".repeat(32),
        status,
        PathBuf::from("/mpla-test/cgroup.procs"),
        NAMESPACE_HOLDER_PID,
        "mnt:[4026532999]".to_owned(),
        4_026_532_999,
    )
    .expect("parse raw process evidence");
    assert_eq!(process.capabilities.effective, 1 << 21);
    assert_eq!(process.capabilities.permitted, 1 << 21);
    assert_eq!(process.capabilities.inheritable, 0);
    assert_eq!(process.capabilities.bounding, (1 << 21) | (1 << 12));
    assert_eq!(process.capabilities.ambient, 0);
    assert_eq!(process.seccomp.profile_id, STORAGE_ADMIN_SECCOMP_PROFILE_ID);
    assert_eq!(process.seccomp.mode, 2);
    assert_eq!(process.seccomp.filter_count, 1);
    assert!(process.seccomp.no_new_privs);

    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-mount-evidence");
    let mount = storage_admin_mount_plan_evidence(&invocation.request.scope)
        .expect("capture exact mount plan");
    assert_eq!(mount.mount_namespace_id, "mnt:[4026532999]");
    assert_eq!(mount.source, "none");
    assert_eq!(mount.filesystem_type, "overlay");
    assert_eq!(mount.target, root.0.join("workspace"));
    assert_eq!(mount.flags, ["MS_NODEV", "MS_NOSUID"]);
    assert_eq!(
        mount.lower_dirs_newest_first,
        [root.0.join("payload/lower-1")]
    );
    assert_eq!(mount.upper_dir, root.0.join("allocation/upper"));
    assert_eq!(mount.work_dir, root.0.join("allocation/work"));
}

#[test]
fn storage_authority_cannot_become_or_spawn_the_workload() {
    let profile = StorageAdminProcessProfile;
    assert!(!profile.allows_workload_entry());
    assert!(!profile.allows_workload_descendants());
    assert!(!profile.allows_arbitrary_executable());
    assert_eq!(profile.effective_capabilities(), &["CAP_SYS_ADMIN"]);
    assert_eq!(
        profile.allowed_privileged_syscalls(),
        &["mount", "umount2", "setns", "syncfs"]
    );
}

#[test]
fn receipt_schema_captures_identity_security_scope_result_cleanup_and_time() {
    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-receipt-schema");
    let mut lifecycle = FakeLifecycle::succeeding();
    let receipt = run_storage_admin(&invocation, &mut lifecycle).expect("receipt");
    let value = serde_json::to_value(&receipt).expect("encode receipt");
    let fields: BTreeSet<_> = value
        .as_object()
        .expect("receipt object")
        .keys()
        .map(String::as_str)
        .collect();
    let expected: BTreeSet<_> = [
        "schema_version",
        "interface_version",
        "profile_id",
        "operation_id",
        "action",
        "request_sha256",
        "trusted_executable",
        "effective_capabilities",
        "allowed_privileged_syscalls",
        "process_evidence",
        "mount_plan_evidence",
        "mount_attestation",
        "mount_receipt_binding",
        "scope",
        "outcome",
        "idempotent_replay",
        "cleanup_complete",
        "failure",
        "started_unix_ms",
        "completed_unix_ms",
        "receipt_path",
    ]
    .into_iter()
    .collect();

    assert_eq!(fields, expected);
    assert_eq!(
        receipt.trusted_executable,
        PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    );
    assert_eq!(receipt.effective_capabilities, vec!["CAP_SYS_ADMIN"]);
    assert_eq!(
        receipt.allowed_privileged_syscalls,
        vec!["mount", "umount2", "setns", "syncfs"]
    );
    assert_eq!(receipt.scope, invocation.request.scope);
    assert_eq!(
        receipt.process_evidence.executable,
        PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    );
    assert_eq!(
        receipt.mount_plan_evidence.target,
        invocation.request.scope.workspace_root
    );
    assert_eq!(receipt.outcome, StorageAdminOutcome::Succeeded);
    assert!(receipt.cleanup_complete);
    assert!(receipt.failure.is_none());
    assert!(receipt.started_unix_ms > 0);
    assert!(receipt.completed_unix_ms >= receipt.started_unix_ms);
    assert_eq!(receipt.request_sha256.len(), 64);
}
