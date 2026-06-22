mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sandbox_protocol::{OperationScope, Request};
use sandbox_runtime::cgroup_monitor::{InspectCgroupMonitorInput, ReadCgroupMonitorSamplesInput};
use sandbox_runtime::command::CommandSessionId;
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::SandboxRuntimeOperations;
use sandbox_runtime_workspace::{DestroyWorkspaceRequest, WorkspaceProfile, WorkspaceSessionId};
use serde_json::{json, Value};

use support::{build_services, create_request, workspace_handle_with_cgroup, FakeWorkspaceService};

#[test]
fn cgroup_monitor_catalog_metadata_is_runtime_family() {
    let catalog = sandbox_runtime::operation_catalog();
    let family = catalog
        .families
        .iter()
        .find(|family| family.id == "cgroup_monitor")
        .expect("cgroup monitor family is exported");
    assert_eq!(family.title, "Cgroup Monitor");

    let inspect = catalog
        .operations
        .iter()
        .find(|spec| spec.name == "inspect_cgroup_monitor")
        .expect("inspect op is exported");
    assert_eq!(inspect.family, "cgroup_monitor");
    assert_eq!(inspect.related, &["read_cgroup_monitor_samples"]);
    assert!(inspect
        .cli
        .expect("inspect cli metadata")
        .usage
        .starts_with("sandbox-cli runtime "));

    let read = catalog
        .operations
        .iter()
        .find(|spec| spec.name == "read_cgroup_monitor_samples")
        .expect("read op is exported");
    assert_eq!(read.family, "cgroup_monitor");
    assert_eq!(read.related, &["inspect_cgroup_monitor"]);
    let read_cli = read.cli.expect("read cli metadata");
    assert!(read_cli.usage.starts_with("sandbox-cli runtime "));
    assert!(!read_cli.usage.contains("--sandbox-id"));
}

#[test]
fn cgroup_monitor_inspect_reads_session_target_without_command_payload(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = CgroupFixture::new("inspect-session")?;
    write_cgroup_files(&fixture.cgroup)?;
    let env = services_with_session(&fixture)?;
    let operations = operations(&env)?;

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "inspect_cgroup_monitor",
            "req-1",
            OperationScope::system(),
            json!({ "workspace_session_id": "ws-cgroup" }),
        ),
    )
    .into_json_value();

    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["workspace_session_id"], "ws-cgroup");
    assert_eq!(response["command_session_id"], serde_json::Value::Null);
    assert_eq!(response["target"]["kind"], "session");
    assert_eq!(response["latest"]["sample_kind"], "periodic");
    assert_eq!(response["latest"]["cpu"]["usage_usec"], 1200);
    assert_forbidden_keys_absent(
        &response,
        &[
            "cmd", "command", "stdin", "stdout", "stderr", "env", "args", "argv",
        ],
    );

    let _ = std::fs::remove_dir_all(fixture.root);
    Ok(())
}

#[test]
fn cgroup_monitor_read_samples_uses_optional_limit_without_offsets(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = CgroupFixture::new("read-samples")?;
    write_cgroup_files(&fixture.cgroup)?;
    let env = services_with_session(&fixture)?;
    let command_cgroup = fixture.cgroup.join("commands").join("cmd-1");
    std::fs::create_dir_all(&command_cgroup)?;
    write_cgroup_files(&command_cgroup)?;

    env.workspace.cgroup_monitor().register_command(
        WorkspaceSessionId("ws-cgroup".to_owned()),
        "cmd-1",
        command_cgroup,
        fixture.upper.clone(),
    );

    let output =
        SandboxRuntimeOperations::new(Arc::clone(&env.command), layerstack_service(&fixture.root)?)
            .cgroup_monitor
            .read_cgroup_monitor_samples(ReadCgroupMonitorSamplesInput {
                workspace_session_id: WorkspaceSessionId("ws-cgroup".to_owned()),
                command_session_id: Some(CommandSessionId("cmd-1".to_owned())),
                limit: Some(1),
            })?;

    assert_eq!(
        output.command_session_id,
        Some(CommandSessionId("cmd-1".to_owned()))
    );
    assert_eq!(output.samples.len(), 1);
    assert!(
        output.samples[0].state.read_error.is_none(),
        "command cgroup sample should be readable: {:?}",
        output.samples[0].state.read_error
    );
    assert_eq!(output.samples[0].cpu.usage_usec, Some(1200));
    assert_eq!(
        output.target.kind,
        sandbox_runtime_workspace::CgroupMonitorTargetKind::Command
    );

    let catalog = sandbox_runtime::operation_catalog();
    let read = catalog
        .operations
        .iter()
        .find(|spec| spec.name == "read_cgroup_monitor_samples")
        .expect("read op is exported");
    let arg_names = read.args.iter().map(|arg| arg.name).collect::<Vec<_>>();
    assert_eq!(
        arg_names,
        ["workspace_session_id", "command_session_id", "limit"]
    );

    let _ = std::fs::remove_dir_all(fixture.root);
    Ok(())
}

#[test]
fn cgroup_monitor_service_faults_missing_command_target(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = CgroupFixture::new("missing-command")?;
    write_cgroup_files(&fixture.cgroup)?;
    let env = services_with_session(&fixture)?;
    let monitor =
        SandboxRuntimeOperations::new(Arc::clone(&env.command), layerstack_service(&fixture.root)?)
            .cgroup_monitor;

    let error = monitor
        .inspect_cgroup_monitor(InspectCgroupMonitorInput {
            workspace_session_id: WorkspaceSessionId("ws-cgroup".to_owned()),
            command_session_id: Some(CommandSessionId("missing".to_owned())),
        })
        .expect_err("missing command target is a normal operation fault");

    assert!(error.to_string().contains("command session"));

    let _ = std::fs::remove_dir_all(fixture.root);
    Ok(())
}

#[test]
fn cgroup_monitor_retained_session_cleanup_remains_readable_after_destroy(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = CgroupFixture::new("destroy-retained")?;
    write_cgroup_files(&fixture.cgroup)?;
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle_with_cgroup(
        "ws-cgroup",
        "lease-1",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
        Some(fixture.cgroup.clone()),
    )));
    let env = build_services(Arc::clone(&fake));
    let handler = env.workspace.create_workspace_session(create_request())?;
    env.workspace
        .destroy_session(handler, DestroyWorkspaceRequest::default())?;
    let monitor =
        SandboxRuntimeOperations::new(Arc::clone(&env.command), layerstack_service(&fixture.root)?)
            .cgroup_monitor;

    let output = monitor.inspect_cgroup_monitor(InspectCgroupMonitorInput {
        workspace_session_id: WorkspaceSessionId("ws-cgroup".to_owned()),
        command_session_id: None,
    })?;

    assert!(output.cleanup.final_sample_recorded);
    assert_eq!(output.cleanup.cgroup_exists_after_destroy, Some(true));
    assert_eq!(
        output.latest.as_ref().map(|sample| sample.sample_kind),
        Some(sandbox_runtime_workspace::CgroupSampleKind::SessionFinal)
    );

    let _ = std::fs::remove_dir_all(fixture.root);
    Ok(())
}

struct CgroupFixture {
    root: PathBuf,
    cgroup: PathBuf,
    upper: PathBuf,
}

impl CgroupFixture {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let root = std::env::temp_dir().join(format!(
            "sandbox-runtime-operation-cgroup-monitor-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let cgroup = root.join("eos").join("sessions").join("ws-cgroup");
        let upper = root.join("upper");
        std::fs::create_dir_all(&cgroup)?;
        std::fs::create_dir_all(&upper)?;
        std::fs::write(upper.join("file.txt"), b"abc")?;
        Ok(Self {
            root,
            cgroup,
            upper,
        })
    }
}

fn services_with_session(
    fixture: &CgroupFixture,
) -> Result<support::TestServices, Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle_with_cgroup(
        "ws-cgroup",
        "lease-1",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
        Some(fixture.cgroup.clone()),
    )));
    let env = build_services(fake);
    env.workspace.create_workspace_session(create_request())?;
    Ok(env)
}

fn operations(
    env: &support::TestServices,
) -> Result<SandboxRuntimeOperations, Box<dyn std::error::Error + Send + Sync>> {
    let root = std::env::temp_dir().join(format!(
        "sandbox-runtime-operation-cgroup-monitor-ops-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    Ok(SandboxRuntimeOperations::new(
        Arc::clone(&env.command),
        layerstack_service(&root)?,
    ))
}

fn layerstack_service(
    root: &Path,
) -> Result<Arc<LayerStackService>, Box<dyn std::error::Error + Send + Sync>> {
    let layerstack_root = root.join("layer-stack");
    let workspace = root.join("layerstack-workspace");
    std::fs::create_dir_all(&workspace)?;
    sandbox_runtime_layerstack::build_workspace_base(&layerstack_root, &workspace, false)?;
    Ok(Arc::new(LayerStackService::new(layerstack_root)?))
}

fn write_cgroup_files(cgroup: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::fs::write(
        cgroup.join("cpu.stat"),
        "usage_usec 1200\nuser_usec 800\nsystem_usec 400\n",
    )?;
    std::fs::write(cgroup.join("memory.current"), "4096\n")?;
    std::fs::write(cgroup.join("memory.peak"), "8192\n")?;
    std::fs::write(
        cgroup.join("memory.stat"),
        "anon 100\nfile 200\nkernel 300\n",
    )?;
    std::fs::write(cgroup.join("memory.events"), "oom 0\noom_kill 0\n")?;
    std::fs::write(
        cgroup.join("io.stat"),
        "8:0 rbytes=10 wbytes=20 rios=1 wios=2\n",
    )?;
    std::fs::write(cgroup.join("pids.current"), "1\n")?;
    std::fs::write(cgroup.join("pids.peak"), "2\n")?;
    std::fs::write(cgroup.join("cgroup.procs"), "123\n")?;
    std::fs::write(
        cgroup.join("cpu.pressure"),
        "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
    )?;
    std::fs::write(
        cgroup.join("memory.pressure"),
        "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
    )?;
    std::fs::write(
        cgroup.join("io.pressure"),
        "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
    )?;
    std::fs::write(cgroup.join("cgroup.events"), "populated 1\nfrozen 0\n")?;
    Ok(())
}

fn assert_forbidden_keys_absent(value: &Value, forbidden: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in map.keys() {
                assert!(
                    !forbidden.contains(&key.as_str()),
                    "forbidden key {key} leaked in {value}"
                );
            }
            for child in map.values() {
                assert_forbidden_keys_absent(child, forbidden);
            }
        }
        Value::Array(values) => {
            for child in values {
                assert_forbidden_keys_absent(child, forbidden);
            }
        }
        _ => {}
    }
}
