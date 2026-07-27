#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use crate::config::{
    MEMORY_HIGH_BYTES, MEMORY_MAX_BYTES, REQUIRED_DOCKER_CPUS, REQUIRED_DOCKER_MEMORY_BYTES,
    REQUIRED_IMAGE_PLATFORM, REQUIRED_IMAGE_RELEASE, RESIDENT_POOL_BYTES,
};
#[cfg(target_os = "linux")]
use crate::evidence::capture_physical_snapshot;
use crate::evidence::{read_json, write_atomic_json};
#[cfg(target_os = "linux")]
use crate::PocError;
use crate::{
    unix_time_ms, ArtifactStatus, EnvironmentReceipt, PhysicalSnapshot, PocConfig, PocResult,
    ProbeReceipt, ProbeStatus, QualificationReceipt, QualificationRequest, INTERFACE_VERSION,
    SCHEMA_VERSION,
};

#[cfg(target_os = "linux")]
const STABLE_SENTINEL: &str = "mpla-stable-sentinel";
#[cfg(target_os = "linux")]
const WRITTEN_SENTINEL: &str = "sm01-mounted-write";
#[cfg(target_os = "linux")]
const WHITEOUT_TARGET: &str = "whiteout-target";
#[cfg(target_os = "linux")]
const OPAQUE_DIRECTORY: &str = "opaque-dir";
#[cfg(target_os = "linux")]
const QUALIFICATION_XATTR: &str = "user.mpla.qualification";

pub fn qualify(
    config: &PocConfig,
    request: &QualificationRequest,
) -> PocResult<QualificationReceipt> {
    config.validate()?;
    let artifact_path = request
        .evidence_root
        .join("environment")
        .join("qualification.json");
    let mut collected = match collect_platform(config, request) {
        Ok(collected) => collected,
        Err(error) => failed_collection(request, error.to_string()),
    };

    let pre_path = request
        .evidence_root
        .join("cases")
        .join("SM-01")
        .join("storage")
        .join("pre.json");
    let post_path = request
        .evidence_root
        .join("cases")
        .join("SM-01")
        .join("storage")
        .join("post.json");
    let pre_result = write_atomic_json(&pre_path, &collected.before)
        .map(|()| pre_path.display().to_string())
        .map_err(|error| error.to_string());
    push_result(
        &mut collected.probes,
        "before_snapshot_durable",
        true,
        Some("atomic JSON snapshot".to_owned()),
        pre_result,
    );
    let post_result = write_atomic_json(&post_path, &collected.after)
        .map(|()| post_path.display().to_string())
        .map_err(|error| error.to_string());
    push_result(
        &mut collected.probes,
        "after_snapshot_durable",
        true,
        Some("atomic JSON snapshot".to_owned()),
        post_result,
    );
    push_passed(
        &mut collected.probes,
        "qualification_json_roundtrip",
        true,
        "pending atomic write and exact decode",
        Some("schema-valid exact round trip".to_owned()),
    );

    let status = qualification_status(&collected.probes);
    let mut receipt = QualificationReceipt {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        run_id: request.run_id.clone(),
        status,
        created_unix_ms: unix_time_ms()?,
        required_image_digest: config.required_image_digest.clone(),
        environment: collected.environment,
        probes: collected.probes,
        before: collected.before,
        after: collected.after,
        artifact_path: artifact_path.clone(),
    };
    write_atomic_json(&artifact_path, &receipt)?;

    match read_json::<QualificationReceipt>(&artifact_path) {
        Ok(decoded) if decoded == receipt => Ok(receipt),
        Ok(_) => {
            fail_roundtrip_probe(&mut receipt, "decoded receipt differs from written receipt");
            write_atomic_json(&artifact_path, &receipt)?;
            Ok(receipt)
        }
        Err(error) => {
            fail_roundtrip_probe(&mut receipt, &error.to_string());
            write_atomic_json(&artifact_path, &receipt)?;
            Ok(receipt)
        }
    }
}

struct CollectedQualification {
    environment: EnvironmentReceipt,
    probes: Vec<ProbeReceipt>,
    before: PhysicalSnapshot,
    after: PhysicalSnapshot,
}

fn qualification_status(probes: &[ProbeReceipt]) -> ArtifactStatus {
    if probes
        .iter()
        .any(|probe| probe.mandatory && probe.status != ProbeStatus::Passed)
    {
        ArtifactStatus::Failed
    } else {
        ArtifactStatus::Passed
    }
}

fn fail_roundtrip_probe(receipt: &mut QualificationReceipt, observed: &str) {
    receipt.status = ArtifactStatus::Failed;
    if let Some(probe) = receipt
        .probes
        .iter_mut()
        .find(|probe| probe.name == "qualification_json_roundtrip")
    {
        probe.status = ProbeStatus::Failed;
        probe.observed = observed.to_owned();
    }
}

fn failed_collection(request: &QualificationRequest, observed: String) -> CollectedQualification {
    let snapshot = empty_snapshot(request);
    CollectedQualification {
        environment: empty_environment(),
        probes: vec![ProbeReceipt {
            name: "qualification_execution".to_owned(),
            mandatory: true,
            status: ProbeStatus::Failed,
            observed,
            expected: Some("complete Linux qualification execution".to_owned()),
        }],
        before: snapshot.clone(),
        after: snapshot,
    }
}

fn empty_environment() -> EnvironmentReceipt {
    EnvironmentReceipt {
        architecture: std::env::consts::ARCH.to_owned(),
        kernel_release: String::new(),
        filesystem_type: String::new(),
        filesystem_mount_options: Vec::new(),
        payload_mount_id: 0,
        control_mount_id: 0,
        cpu_count: 0,
        memory_bytes: 0,
        free_bytes: 0,
        free_inodes: 0,
    }
}

fn empty_snapshot(request: &QualificationRequest) -> PhysicalSnapshot {
    PhysicalSnapshot {
        allocation_id: request.allocation_id.clone(),
        allocation_path: request.allocation_root.clone(),
        device: 0,
        representative_inodes: Vec::new(),
        logical_bytes: 0,
        allocated_bytes: 0,
        inode_count: 0,
        file_count: 0,
        directory_count: 0,
    }
}

fn push_passed(
    probes: &mut Vec<ProbeReceipt>,
    name: &str,
    mandatory: bool,
    observed: impl Into<String>,
    expected: Option<String>,
) {
    probes.push(ProbeReceipt {
        name: name.to_owned(),
        mandatory,
        status: ProbeStatus::Passed,
        observed: observed.into(),
        expected,
    });
}

fn push_failed(
    probes: &mut Vec<ProbeReceipt>,
    name: &str,
    mandatory: bool,
    observed: impl Into<String>,
    expected: Option<String>,
) {
    probes.push(ProbeReceipt {
        name: name.to_owned(),
        mandatory,
        status: ProbeStatus::Failed,
        observed: observed.into(),
        expected,
    });
}

#[cfg(target_os = "linux")]
fn push_optional_unavailable(
    probes: &mut Vec<ProbeReceipt>,
    name: &str,
    observed: impl Into<String>,
) {
    probes.push(ProbeReceipt {
        name: name.to_owned(),
        mandatory: false,
        status: ProbeStatus::OptionalUnavailable,
        observed: observed.into(),
        expected: None,
    });
}

fn push_result(
    probes: &mut Vec<ProbeReceipt>,
    name: &str,
    mandatory: bool,
    expected: Option<String>,
    result: Result<String, String>,
) {
    match result {
        Ok(observed) => push_passed(probes, name, mandatory, observed, expected),
        Err(observed) => push_failed(probes, name, mandatory, observed, expected),
    }
}

#[cfg(target_os = "linux")]
fn check(
    condition: bool,
    observed: impl Into<String>,
    failure: impl Into<String>,
) -> Result<String, String> {
    if condition {
        Ok(observed.into())
    } else {
        Err(failure.into())
    }
}

#[cfg(not(target_os = "linux"))]
fn collect_platform(
    _config: &PocConfig,
    request: &QualificationRequest,
) -> PocResult<CollectedQualification> {
    let snapshot = empty_snapshot(request);
    Ok(CollectedQualification {
        environment: empty_environment(),
        probes: vec![ProbeReceipt {
            name: "linux_runtime".to_owned(),
            mandatory: true,
            status: ProbeStatus::Failed,
            observed: format!(
                "unsupported host operating system: {}",
                std::env::consts::OS
            ),
            expected: Some("linux".to_owned()),
        }],
        before: snapshot.clone(),
        after: snapshot,
    })
}

#[cfg(target_os = "linux")]
fn collect_platform(
    config: &PocConfig,
    request: &QualificationRequest,
) -> PocResult<CollectedQualification> {
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;

    use rustix::fs::XattrFlags;
    use rustix::process::{pidfd_open, Pid, PidfdFlags};
    use sandbox_runtime_overlay::{mount_overlay, OverlayHandle};

    let mut probes = Vec::new();
    push_result(
        &mut probes,
        "request_schema",
        true,
        Some(SCHEMA_VERSION.to_string()),
        check(
            request.schema_version == SCHEMA_VERSION,
            request.schema_version.to_string(),
            format!("observed schema version {}", request.schema_version),
        ),
    );

    let expected_allocation_root = request
        .payload_root
        .join("allocations")
        .join(request.allocation_id.as_str());
    push_result(
        &mut probes,
        "permanent_allocation_path",
        true,
        Some(expected_allocation_root.display().to_string()),
        check(
            request.allocation_root == expected_allocation_root,
            request.allocation_root.display().to_string(),
            format!(
                "allocation root {} is not the permanent payload arena path {}",
                request.allocation_root.display(),
                expected_allocation_root.display()
            ),
        ),
    );
    push_result(
        &mut probes,
        "workspace_control_path",
        true,
        Some(format!("descendant of {}", request.control_root.display())),
        check(
            request.workspace_root.starts_with(&request.control_root),
            request.workspace_root.display().to_string(),
            format!(
                "workspace {} is outside control root {}",
                request.workspace_root.display(),
                request.control_root.display()
            ),
        ),
    );
    push_result(
        &mut probes,
        "fixture_lower_path",
        true,
        Some(format!("descendant of {}", request.fixtures_root.display())),
        check(
            request.lower_dir.starts_with(&request.fixtures_root),
            request.lower_dir.display().to_string(),
            format!(
                "lower {} is outside fixtures root {}",
                request.lower_dir.display(),
                request.fixtures_root.display()
            ),
        ),
    );

    require_directory(&request.payload_root, "payload root")?;
    require_directory(&request.control_root, "control root")?;
    require_directory(&request.fixtures_root, "fixtures root")?;
    require_directory(&request.lower_dir, "fixture lower")?;
    fs::create_dir_all(&request.allocation_root).map_err(|error| {
        PocError::io(
            "create permanent allocation root",
            &request.allocation_root,
            error,
        )
    })?;
    let upper_dir = request.allocation_root.join("upper");
    let work_dir = request.allocation_root.join("work");
    fs::create_dir_all(&upper_dir)
        .map_err(|error| PocError::io("create permanent upper", &upper_dir, error))?;
    fs::create_dir_all(&work_dir)
        .map_err(|error| PocError::io("create adjacent workdir", &work_dir, error))?;
    fs::create_dir_all(&request.workspace_root).map_err(|error| {
        PocError::io(
            "create workspace mountpoint",
            &request.workspace_root,
            error,
        )
    })?;

    let adjacency = upper_dir.parent() == work_dir.parent()
        && upper_dir.parent() == Some(request.allocation_root.as_path());
    push_result(
        &mut probes,
        "permanent_upper_adjacent_workdir",
        true,
        Some(format!(
            "{}/{{upper,work}}",
            request.allocation_root.display()
        )),
        check(
            adjacency,
            format!("upper={} work={}", upper_dir.display(), work_dir.display()),
            format!(
                "upper {} and work {} are not adjacent under allocation root",
                upper_dir.display(),
                work_dir.display()
            ),
        ),
    );

    let workspace_empty = fs::read_dir(&request.workspace_root)
        .map_err(|error| PocError::io("read workspace mountpoint", &request.workspace_root, error))?
        .next()
        .is_none();
    push_result(
        &mut probes,
        "workspace_mountpoint_empty",
        true,
        Some("empty directory before mount".to_owned()),
        check(
            workspace_empty,
            request.workspace_root.display().to_string(),
            format!(
                "workspace mountpoint is not empty: {}",
                request.workspace_root.display()
            ),
        ),
    );

    for fixture in [
        request.lower_dir.join(WHITEOUT_TARGET),
        request.lower_dir.join(OPAQUE_DIRECTORY).join("lower-entry"),
    ] {
        if !fixture.exists() {
            return Err(PocError::Integrity(format!(
                "required SM-01 fixture is missing: {}",
                fixture.display()
            )));
        }
    }

    let stable_sentinel = upper_dir.join(STABLE_SENTINEL);
    let mut sentinel_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&stable_sentinel)
        .map_err(|error| PocError::io("create stable upper sentinel", &stable_sentinel, error))?;
    writeln!(
        sentinel_file,
        "allocation={} run={}",
        request.allocation_id, request.run_id
    )
    .map_err(|error| PocError::io("write stable upper sentinel", &stable_sentinel, error))?;
    sentinel_file
        .sync_all()
        .map_err(|error| PocError::io("fsync stable upper sentinel", &stable_sentinel, error))?;
    File::open(&upper_dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| PocError::io("fsync permanent upper", &upper_dir, error))?;

    let before = capture_physical_snapshot(&request.allocation_id, &request.allocation_root)?;
    let mountinfo = read_mountinfo()?;
    let payload_mount = find_mount(&mountinfo, &request.payload_root).ok_or_else(|| {
        PocError::Integrity(format!(
            "no mountinfo entry covers payload root {}",
            request.payload_root.display()
        ))
    })?;
    let control_mount = find_mount(&mountinfo, &request.control_root).ok_or_else(|| {
        PocError::Integrity(format!(
            "no mountinfo entry covers control root {}",
            request.control_root.display()
        ))
    })?;
    push_result(
        &mut probes,
        "mountinfo_available",
        true,
        Some("parseable /proc/self/mountinfo".to_owned()),
        check(
            !mountinfo.is_empty(),
            format!("{} parsed mount records", mountinfo.len()),
            "no parseable mountinfo records",
        ),
    );
    push_result(
        &mut probes,
        "payload_control_mount_separation",
        true,
        Some("distinct nonzero mount IDs".to_owned()),
        check(
            payload_mount.mount_id != 0
                && control_mount.mount_id != 0
                && payload_mount.mount_id != control_mount.mount_id,
            format!(
                "payload={} control={}",
                payload_mount.mount_id, control_mount.mount_id
            ),
            format!(
                "payload and control mount IDs are not distinct: {} and {}",
                payload_mount.mount_id, control_mount.mount_id
            ),
        ),
    );

    let architecture = normalized_architecture();
    let platform = format!("linux/{architecture}");
    push_result(
        &mut probes,
        "image_platform",
        true,
        Some(REQUIRED_IMAGE_PLATFORM.to_owned()),
        check(
            platform == config.required_image_platform,
            platform.clone(),
            format!("observed image platform {platform}"),
        ),
    );
    let os_release = read_os_release()?;
    let release_observed = os_release
        .get("PRETTY_NAME")
        .cloned()
        .unwrap_or_else(|| "unknown".to_owned());
    let release_matches = os_release
        .get("NAME")
        .is_some_and(|value| value == "Ubuntu")
        && os_release
            .get("VERSION_ID")
            .is_some_and(|value| value == "24.04");
    push_result(
        &mut probes,
        "image_release",
        true,
        Some(REQUIRED_IMAGE_RELEASE.to_owned()),
        check(
            release_matches,
            release_observed.clone(),
            format!("observed image release {release_observed}"),
        ),
    );
    let digest_observed = std::env::var("MPLA_POC_IMAGE_DIGEST")
        .unwrap_or_else(|_| "MPLA_POC_IMAGE_DIGEST is unset".to_owned());
    push_result(
        &mut probes,
        "image_digest",
        true,
        Some(config.required_image_digest.clone()),
        check(
            digest_observed == config.required_image_digest,
            digest_observed.clone(),
            format!("observed image digest {digest_observed}"),
        ),
    );

    let cpu_count = required_env_number::<u16>("MPLA_POC_DOCKER_CPUS")?;
    push_result(
        &mut probes,
        "docker_cpu_allocation",
        true,
        Some(REQUIRED_DOCKER_CPUS.to_string()),
        check(
            cpu_count == config.docker_cpus,
            format!("Docker Desktop configured CPUs={cpu_count}"),
            format!("observed Docker Desktop CPUs={cpu_count}"),
        ),
    );
    let available_cpus = std::thread::available_parallelism()
        .map_err(|error| PocError::Unsupported(format!("CPU count unavailable: {error}")))
        .and_then(|count| {
            u16::try_from(count.get())
                .map_err(|_| PocError::Integrity("CPU count does not fit u16".to_owned()))
        })?;
    push_result(
        &mut probes,
        "cgroup_cpu_capacity",
        true,
        Some(REQUIRED_DOCKER_CPUS.to_string()),
        check(
            available_cpus == config.docker_cpus,
            available_cpus.to_string(),
            format!("observed {available_cpus} available CPUs"),
        ),
    );

    let memory_bytes = required_env_number::<u64>("MPLA_POC_DOCKER_MEMORY_BYTES")?;
    push_result(
        &mut probes,
        "docker_memory_allocation",
        true,
        Some(REQUIRED_DOCKER_MEMORY_BYTES.to_string()),
        check(
            memory_bytes == config.docker_memory_bytes,
            format!("Docker Desktop configured bytes={memory_bytes}"),
            format!("observed Docker Desktop configured bytes={memory_bytes}"),
        ),
    );
    let cgroup_dir = self_cgroup_dir()?;
    let (memory_high, memory_max) = cgroup_memory_limits(&cgroup_dir)?;
    push_result(
        &mut probes,
        "cgroup_memory_envelope",
        true,
        Some(format!(
            "memory.high={MEMORY_HIGH_BYTES} memory.max={MEMORY_MAX_BYTES}"
        )),
        check(
            memory_high == config.memory_high_bytes && memory_max == config.memory_max_bytes,
            format!("memory.high={memory_high} memory.max={memory_max}"),
            format!("memory.high={memory_high} memory.max={memory_max}"),
        ),
    );
    push_result(
        &mut probes,
        "resident_pool_envelope",
        true,
        Some(RESIDENT_POOL_BYTES.to_string()),
        check(
            config.resident_pool_bytes == RESIDENT_POOL_BYTES,
            format!("resident_pool_bytes={}", config.resident_pool_bytes),
            format!(
                "resident pool is {} bytes, expected {RESIDENT_POOL_BYTES}",
                config.resident_pool_bytes
            ),
        ),
    );
    push_result(
        &mut probes,
        "cgroup_v2",
        true,
        Some("cgroup2 with controllers file".to_owned()),
        check(
            cgroup_dir.join("cgroup.controllers").is_file()
                && mountinfo
                    .iter()
                    .any(|entry| entry.filesystem_type == "cgroup2"),
            cgroup_dir.display().to_string(),
            format!("cgroup v2 unavailable at {}", cgroup_dir.display()),
        ),
    );
    push_result(
        &mut probes,
        "cgroup_memory",
        true,
        Some("memory.current, memory.high, memory.max".to_owned()),
        required_files(
            &cgroup_dir,
            &["memory.current", "memory.high", "memory.max"],
        ),
    );
    push_result(
        &mut probes,
        "cgroup_oom",
        true,
        Some("oom and oom_kill counters available and zero".to_owned()),
        oom_counters(&cgroup_dir),
    );
    push_result(
        &mut probes,
        "cgroup_cpu",
        true,
        Some("cpu.stat and cpu.max".to_owned()),
        required_files(&cgroup_dir, &["cpu.stat", "cpu.max"]),
    );
    push_result(
        &mut probes,
        "cgroup_io",
        true,
        Some("io.stat".to_owned()),
        required_files(&cgroup_dir, &["io.stat"]),
    );

    let raw_pid = i32::try_from(std::process::id())
        .map_err(|_| PocError::Integrity("PID overflow".to_owned()))?;
    let pid = Pid::from_raw(raw_pid)
        .ok_or_else(|| PocError::Integrity("current process has an invalid PID".to_owned()))?;
    push_result(
        &mut probes,
        "pidfd",
        true,
        Some("pidfd_open(current process) succeeds".to_owned()),
        pidfd_open(pid, PidfdFlags::empty())
            .map(|_| format!("pidfd_open({raw_pid}) succeeded"))
            .map_err(|error| error.to_string()),
    );

    let filesystem = rustix::fs::statvfs(&request.payload_root)
        .map_err(|error| PocError::Unsupported(format!("statvfs failed: {error}")))?;
    let allocation_unit = if filesystem.f_frsize == 0 {
        filesystem.f_bsize
    } else {
        filesystem.f_frsize
    };
    let free_bytes = filesystem
        .f_bavail
        .checked_mul(allocation_unit)
        .ok_or_else(|| PocError::Integrity("free-byte accounting overflow".to_owned()))?;
    let free_inodes = filesystem.f_favail;
    push_result(
        &mut probes,
        "filesystem_capacity",
        true,
        Some("nonzero free bytes and free inodes".to_owned()),
        check(
            free_bytes > 0 && free_inodes > 0,
            format!("free_bytes={free_bytes} free_inodes={free_inodes}"),
            format!("free_bytes={free_bytes} free_inodes={free_inodes}"),
        ),
    );

    let handle = OverlayHandle {
        upperdir: upper_dir.clone(),
        workdir: work_dir.clone(),
        layer_paths: vec![request.lower_dir.clone()],
    };
    match mount_overlay(&request.workspace_root, &handle) {
        Ok(mount) => {
            let mounted_info = read_mountinfo().and_then(|entries| {
                let entry =
                    find_exact_mount(&entries, &request.workspace_root).ok_or_else(|| {
                        PocError::Integrity(format!(
                            "workspace mount missing from mountinfo: {}",
                            request.workspace_root.display()
                        ))
                    })?;
                if entry.filesystem_type == "overlay" {
                    Ok(format!(
                        "mount_id={} filesystem={}",
                        entry.mount_id, entry.filesystem_type
                    ))
                } else {
                    Err(PocError::Integrity(format!(
                        "workspace filesystem is {}",
                        entry.filesystem_type
                    )))
                }
            });
            push_result(
                &mut probes,
                "real_overlay_mount",
                true,
                Some("filesystem type overlay".to_owned()),
                mounted_info.map_err(|error| error.to_string()),
            );

            let visible_sentinel = request.workspace_root.join(STABLE_SENTINEL);
            let mounted_write = (|| -> Result<String, String> {
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(&visible_sentinel)
                    .map_err(|error| error.to_string())?;
                writeln!(file, "mounted=true").map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())?;
                let written = request.workspace_root.join(WRITTEN_SENTINEL);
                let mut written_file = File::create(&written).map_err(|error| error.to_string())?;
                written_file
                    .write_all(b"stationary-overlay-write\n")
                    .map_err(|error| error.to_string())?;
                written_file.sync_all().map_err(|error| error.to_string())?;
                Ok(format!(
                    "updated {} and created {}",
                    visible_sentinel.display(),
                    written.display()
                ))
            })();
            push_result(
                &mut probes,
                "overlay_write",
                true,
                Some("write reaches permanent upper".to_owned()),
                mounted_write,
            );

            let whiteout_result = fs::remove_file(request.workspace_root.join(WHITEOUT_TARGET))
                .map(|()| "lower-only file removed from merged view".to_owned())
                .map_err(|error| error.to_string());
            push_result(
                &mut probes,
                "overlay_whiteout_operation",
                true,
                Some("lower-only file deletion succeeds".to_owned()),
                whiteout_result,
            );

            let opaque_result = (|| -> Result<String, String> {
                let path = request.workspace_root.join(OPAQUE_DIRECTORY);
                fs::remove_dir_all(&path).map_err(|error| error.to_string())?;
                fs::create_dir(&path).map_err(|error| error.to_string())?;
                fs::write(path.join("upper-entry"), b"upper\n")
                    .map_err(|error| error.to_string())?;
                Ok(format!("replaced {}", path.display()))
            })();
            push_result(
                &mut probes,
                "overlay_opaque_operation",
                true,
                Some("lower directory replaced by upper-only directory".to_owned()),
                opaque_result,
            );

            let xattr_result = (|| -> Result<String, String> {
                rustix::fs::setxattr(
                    &visible_sentinel,
                    QUALIFICATION_XATTR,
                    b"m0",
                    XattrFlags::empty(),
                )
                .map_err(|error| error.to_string())?;
                let mut value = [0u8; 16];
                let size = rustix::fs::getxattr(&visible_sentinel, QUALIFICATION_XATTR, &mut value)
                    .map_err(|error| error.to_string())?;
                check(
                    &value[..size] == b"m0",
                    format!("{QUALIFICATION_XATTR}=m0"),
                    format!("{QUALIFICATION_XATTR} had unexpected value"),
                )
            })();
            push_result(
                &mut probes,
                "overlay_xattr_operation",
                true,
                Some(format!("{QUALIFICATION_XATTR}=m0")),
                xattr_result,
            );

            let unmount_result = mount
                .unmount()
                .map(|()| "strict unmount succeeded".to_owned())
                .map_err(|error| error.to_string());
            push_result(
                &mut probes,
                "strict_unmount",
                true,
                Some("non-lazy unmount succeeds".to_owned()),
                unmount_result,
            );
        }
        Err(error) => {
            push_failed(
                &mut probes,
                "real_overlay_mount",
                true,
                error.to_string(),
                Some("filesystem type overlay".to_owned()),
            );
            for name in [
                "overlay_write",
                "overlay_whiteout_operation",
                "overlay_opaque_operation",
                "overlay_xattr_operation",
                "strict_unmount",
            ] {
                push_failed(
                    &mut probes,
                    name,
                    true,
                    "not executed because overlay mount failed",
                    None,
                );
            }
        }
    }

    let post_mountinfo = read_mountinfo()?;
    push_result(
        &mut probes,
        "mount_absent_after_unmount",
        true,
        Some("workspace has no mountinfo entry".to_owned()),
        check(
            find_exact_mount(&post_mountinfo, &request.workspace_root).is_none(),
            request.workspace_root.display().to_string(),
            format!(
                "workspace remains mounted after strict unmount: {}",
                request.workspace_root.display()
            ),
        ),
    );
    push_result(
        &mut probes,
        "whiteout_carrier",
        true,
        Some("real overlay whiteout carrier in permanent upper".to_owned()),
        verify_whiteout(&upper_dir.join(WHITEOUT_TARGET)),
    );
    push_result(
        &mut probes,
        "opaque_directory_carrier",
        true,
        Some("overlay opaque xattr in permanent upper".to_owned()),
        verify_opaque(&upper_dir.join(OPAQUE_DIRECTORY)),
    );
    push_result(
        &mut probes,
        "upper_xattr_persisted",
        true,
        Some(format!("{QUALIFICATION_XATTR}=m0")),
        verify_xattr(&stable_sentinel, QUALIFICATION_XATTR, b"m0"),
    );
    push_result(
        &mut probes,
        "upper_write_persisted",
        true,
        Some(format!("upper/{WRITTEN_SENTINEL}")),
        check(
            upper_dir.join(WRITTEN_SENTINEL).is_file(),
            upper_dir.join(WRITTEN_SENTINEL).display().to_string(),
            format!(
                "mounted write missing from permanent upper: {}",
                upper_dir.join(WRITTEN_SENTINEL).display()
            ),
        ),
    );

    let allocation_fd = File::open(&request.allocation_root).map_err(|error| {
        PocError::io(
            "open allocation for syncfs",
            &request.allocation_root,
            error,
        )
    })?;
    push_result(
        &mut probes,
        "syncfs",
        true,
        Some("syncfs(allocation filesystem) succeeds".to_owned()),
        rustix::fs::syncfs(&allocation_fd)
            .map(|()| request.allocation_root.display().to_string())
            .map_err(|error| error.to_string()),
    );

    let after = capture_physical_snapshot(&request.allocation_id, &request.allocation_root)?;
    push_result(
        &mut probes,
        "stable_allocation_device",
        true,
        Some(before.device.to_string()),
        check(
            before.device == after.device && before.allocation_path == after.allocation_path,
            format!(
                "device={} path={}",
                after.device,
                after.allocation_path.display()
            ),
            format!(
                "allocation moved: before device/path {}/{} after {}/{}",
                before.device,
                before.allocation_path.display(),
                after.device,
                after.allocation_path.display()
            ),
        ),
    );
    let sentinel_relative = Path::new("upper").join(STABLE_SENTINEL);
    let before_sentinel = witness(&before, &sentinel_relative);
    let after_sentinel = witness(&after, &sentinel_relative);
    push_result(
        &mut probes,
        "stable_permanent_upper_sentinel",
        true,
        Some("unchanged (st_dev, st_ino) across mount/write/unmount".to_owned()),
        match (before_sentinel, after_sentinel) {
            (Some(before), Some(after))
                if before.device == after.device && before.inode == after.inode =>
            {
                Ok(format!("device={} inode={}", after.device, after.inode))
            }
            (Some(before), Some(after)) => Err(format!(
                "sentinel identity changed from ({},{}) to ({},{})",
                before.device, before.inode, after.device, after.inode
            )),
            _ => Err("stable upper sentinel missing from before or after snapshot".to_owned()),
        },
    );

    let fanotify_control = Path::new("/proc/sys/fs/fanotify/max_user_groups");
    if fanotify_control.is_file() {
        push_passed(
            &mut probes,
            "fanotify",
            false,
            format!(
                "kernel fanotify control present at {}",
                fanotify_control.display()
            ),
            None,
        );
    } else {
        push_optional_unavailable(
            &mut probes,
            "fanotify",
            "kernel fanotify control is unavailable; physical snapshots remain mandatory",
        );
    }

    let mut filesystem_mount_options = payload_mount.mount_options.clone();
    filesystem_mount_options.extend(payload_mount.super_options.clone());
    filesystem_mount_options.sort();
    filesystem_mount_options.dedup();
    let kernel_release = fs::read_to_string("/proc/sys/kernel/osrelease")
        .map_err(|error| PocError::io("read kernel release", "/proc/sys/kernel/osrelease", error))?
        .trim()
        .to_owned();
    let environment = EnvironmentReceipt {
        architecture,
        kernel_release,
        filesystem_type: payload_mount.filesystem_type.clone(),
        filesystem_mount_options,
        payload_mount_id: payload_mount.mount_id,
        control_mount_id: control_mount.mount_id,
        cpu_count,
        memory_bytes,
        free_bytes,
        free_inodes,
    };

    Ok(CollectedQualification {
        environment,
        probes,
        before,
        after,
    })
}

#[cfg(target_os = "linux")]
fn require_directory(path: &Path, label: &str) -> PocResult<()> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| PocError::io("stat required directory", path, error))?;
    if metadata.is_dir() {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "{label} is not a directory: {}",
            path.display()
        )))
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
struct MountEntry {
    mount_id: u64,
    mount_point: PathBuf,
    mount_options: Vec<String>,
    filesystem_type: String,
    super_options: Vec<String>,
}

#[cfg(target_os = "linux")]
fn read_mountinfo() -> PocResult<Vec<MountEntry>> {
    let contents = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| PocError::io("read mountinfo", "/proc/self/mountinfo", error))?;
    let entries = contents.lines().filter_map(parse_mountinfo_line).collect();
    Ok(entries)
}

#[cfg(target_os = "linux")]
fn parse_mountinfo_line(line: &str) -> Option<MountEntry> {
    let (left, right) = line.split_once(" - ")?;
    let left_fields = left.split_whitespace().collect::<Vec<_>>();
    let right_fields = right.split_whitespace().collect::<Vec<_>>();
    if left_fields.len() < 6 || right_fields.len() < 3 {
        return None;
    }
    Some(MountEntry {
        mount_id: left_fields[0].parse().ok()?,
        mount_point: PathBuf::from(unescape_mountinfo(left_fields[4])),
        mount_options: left_fields[5].split(',').map(str::to_owned).collect(),
        filesystem_type: right_fields[0].to_owned(),
        super_options: right_fields[2].split(',').map(str::to_owned).collect(),
    })
}

#[cfg(target_os = "linux")]
fn unescape_mountinfo(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..=index + 3].iter().all(u8::is_ascii_digit)
        {
            let octal = (bytes[index + 1] - b'0') * 64
                + (bytes[index + 2] - b'0') * 8
                + (bytes[index + 3] - b'0');
            decoded.push(octal);
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(target_os = "linux")]
fn find_mount<'a>(entries: &'a [MountEntry], path: &Path) -> Option<&'a MountEntry> {
    let canonical = std::fs::canonicalize(path).ok()?;
    entries
        .iter()
        .filter(|entry| canonical == entry.mount_point || canonical.starts_with(&entry.mount_point))
        .max_by_key(|entry| entry.mount_point.as_os_str().len())
}

#[cfg(target_os = "linux")]
fn find_exact_mount<'a>(entries: &'a [MountEntry], path: &Path) -> Option<&'a MountEntry> {
    let canonical = std::fs::canonicalize(path).ok()?;
    entries.iter().find(|entry| entry.mount_point == canonical)
}

#[cfg(target_os = "linux")]
fn normalized_architecture() -> String {
    match std::env::consts::ARCH {
        "aarch64" => "arm64".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(target_os = "linux")]
fn read_os_release() -> PocResult<std::collections::HashMap<String, String>> {
    let path = Path::new("/etc/os-release");
    let contents = std::fs::read_to_string(path)
        .map_err(|error| PocError::io("read image release", path, error))?;
    Ok(contents
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.to_owned(), value.trim_matches('"').to_owned()))
        })
        .collect())
}

#[cfg(target_os = "linux")]
fn self_cgroup_dir() -> PocResult<PathBuf> {
    let path = Path::new("/proc/self/cgroup");
    let contents = std::fs::read_to_string(path)
        .map_err(|error| PocError::io("read self cgroup", path, error))?;
    let relative = contents
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| PocError::Unsupported("unified cgroup v2 membership missing".to_owned()))?;
    Ok(Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
}

#[cfg(target_os = "linux")]
fn cgroup_memory_limits(cgroup_dir: &Path) -> PocResult<(u64, u64)> {
    Ok((
        read_finite_cgroup_value(&cgroup_dir.join("memory.high"))?,
        read_finite_cgroup_value(&cgroup_dir.join("memory.max"))?,
    ))
}

#[cfg(target_os = "linux")]
fn read_finite_cgroup_value(path: &Path) -> PocResult<u64> {
    let value = std::fs::read_to_string(path)
        .map_err(|error| PocError::io("read finite cgroup value", path, error))?;
    value.trim().parse::<u64>().map_err(|error| {
        PocError::Unsupported(format!(
            "finite cgroup value is required at {}: {error}",
            path.display()
        ))
    })
}

#[cfg(target_os = "linux")]
fn required_env_number<T>(name: &str) -> PocResult<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = std::env::var(name).map_err(|_| {
        PocError::Unsupported(format!("required environment value {name} is unset"))
    })?;
    raw.parse::<T>().map_err(|error| {
        PocError::Integrity(format!(
            "invalid numeric environment value {name}={raw}: {error}"
        ))
    })
}

#[cfg(target_os = "linux")]
fn required_files(cgroup_dir: &Path, names: &[&str]) -> Result<String, String> {
    let missing = names
        .iter()
        .filter(|name| !cgroup_dir.join(name).is_file())
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(names.join(","))
    } else {
        Err(format!(
            "missing cgroup files at {}: {}",
            cgroup_dir.display(),
            missing.join(",")
        ))
    }
}

#[cfg(target_os = "linux")]
fn oom_counters(cgroup_dir: &Path) -> Result<String, String> {
    let path = cgroup_dir.join("memory.events");
    let contents = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let values = contents
        .lines()
        .filter_map(|line| line.split_once(' '))
        .collect::<std::collections::HashMap<_, _>>();
    let oom = values
        .get("oom")
        .ok_or_else(|| "oom counter missing".to_owned())?
        .parse::<u64>()
        .map_err(|error| error.to_string())?;
    let oom_kill = values
        .get("oom_kill")
        .ok_or_else(|| "oom_kill counter missing".to_owned())?
        .parse::<u64>()
        .map_err(|error| error.to_string())?;
    check(
        oom == 0 && oom_kill == 0,
        format!("oom={oom} oom_kill={oom_kill}"),
        format!("nonzero OOM counters: oom={oom} oom_kill={oom_kill}"),
    )
}

#[cfg(target_os = "linux")]
fn verify_whiteout(path: &Path) -> Result<String, String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_char_device() && metadata.rdev() == 0 {
            return Ok(format!("character-device whiteout {}", path.display()));
        }
    }
    for name in ["user.overlay.whiteout", "trusted.overlay.whiteout"] {
        let mut value = [0u8; 8];
        if let Ok(size) = rustix::fs::lgetxattr(path, name, &mut value) {
            return Ok(format!(
                "{}={} at {}",
                name,
                String::from_utf8_lossy(&value[..size]),
                path.display()
            ));
        }
    }
    Err(format!(
        "no character-device or overlay xattr whiteout at {}",
        path.display()
    ))
}

#[cfg(target_os = "linux")]
fn verify_opaque(path: &Path) -> Result<String, String> {
    for name in ["user.overlay.opaque", "trusted.overlay.opaque"] {
        let mut value = [0u8; 8];
        if let Ok(size) = rustix::fs::lgetxattr(path, name, &mut value) {
            if size > 0 {
                return Ok(format!(
                    "{}={} at {}",
                    name,
                    String::from_utf8_lossy(&value[..size]),
                    path.display()
                ));
            }
        }
    }
    Err(format!("no overlay opaque xattr at {}", path.display()))
}

#[cfg(target_os = "linux")]
fn verify_xattr(path: &Path, name: &str, expected: &[u8]) -> Result<String, String> {
    let mut value = vec![0u8; expected.len().saturating_add(16)];
    let size = rustix::fs::lgetxattr(path, name, &mut value).map_err(|error| error.to_string())?;
    check(
        &value[..size] == expected,
        format!("{name}={}", String::from_utf8_lossy(expected)),
        format!("{name} had unexpected value at {}", path.display()),
    )
}

#[cfg(target_os = "linux")]
fn witness<'a>(
    snapshot: &'a PhysicalSnapshot,
    relative_path: &Path,
) -> Option<&'a crate::InodeWitness> {
    snapshot
        .representative_inodes
        .iter()
        .find(|witness| witness.relative_path == relative_path)
}
