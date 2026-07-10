use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::yaml::Value;
use super::*;

#[test]
fn load_path_reads_committed_baseline() {
    let path = ConfigPath::prd().expect("resolve prd config");
    let doc = load_path(path.as_path()).expect("prd.yml loads");
    let section = doc
        .section::<Value>("daemon")
        .expect("daemon section exists");

    assert!(matches!(section, Value::Mapping(_)));
}

#[test]
fn merge_recurses_objects_replaces_scalars_and_replaces_arrays() {
    let mut baseline = parse_doc(
        r#"
runtime:
  namespace_execution:
    scratch_root: /eos/namespace_execution
runner:
  mount_mask:
    hidden_paths: [/eos, /tmp/eos]
"#,
    );
    let override_doc = parse_doc(
        r#"
runtime:
  namespace_execution:
    scratch_root: /tmp/eos/namespace_execution
runner:
  mount_mask:
    hidden_paths: [/eos]
"#,
    );

    baseline.merge(override_doc).expect("merge succeeds");

    let runtime = baseline
        .section::<Value>("runtime")
        .expect("runtime section deserializes");
    let runner = baseline
        .section::<Value>("runner")
        .expect("runner section deserializes");
    assert_eq!(
        runtime["namespace_execution"]["scratch_root"],
        Value::String("/tmp/eos/namespace_execution".to_owned())
    );
    assert_eq!(
        runner["mount_mask"]["hidden_paths"],
        Value::Sequence(vec![Value::String("/eos".to_owned())])
    );
}

#[test]
fn section_reports_unknown_field_errors() {
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictSection {
        #[serde(rename = "expected")]
        _expected: u64,
    }

    let doc = parse_doc(
        r#"
daemon:
  expected: 1
  unexpected: true
"#,
    );

    let err = doc
        .section::<StrictSection>("daemon")
        .expect_err("unknown field should fail");
    let message = err.to_string();

    assert!(message.contains("daemon"), "{message}");
    assert!(message.contains("unexpected"), "{message}");
}

#[test]
fn section_reports_wrong_type_errors() {
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictSection {
        #[serde(rename = "expected")]
        _expected: u64,
    }

    let doc = parse_doc(
        r#"
daemon:
  expected: wrong
"#,
    );

    let err = doc
        .section::<StrictSection>("daemon")
        .expect_err("wrong field type should fail");
    let message = err.to_string();

    assert!(message.contains("daemon"), "{message}");
    assert!(message.contains("expected"), "{message}");
}

#[test]
fn load_test_override_merges_sandbox_local_test_yaml() {
    let root = test_workspace_dir("load-test-override");
    fs::create_dir_all(&root).expect("create test dir");
    let override_path = root.join("local.test.yml");
    fs::write(
        &override_path,
        r#"
runner:
  mount_mask:
    hidden_paths: [/tmp/eos]
"#,
    )
    .expect("write override");

    let doc = load_test_override(&override_path).expect("override loads");
    let section = doc.section::<Value>("runner").expect("section loads");

    assert_eq!(
        section["mount_mask"]["hidden_paths"],
        Value::Sequence(vec![Value::String("/tmp/eos".to_owned())])
    );
    assert!(matches!(section["mount_mask"], Value::Mapping(_)));
}

#[test]
fn load_test_override_rejects_non_test_yml_path() {
    let root = test_workspace_dir("non-test-yml");
    fs::create_dir_all(&root).expect("create test dir");
    let override_path = root.join("local.yml");
    fs::write(&override_path, "version: 1\n").expect("write override");

    let err = load_test_override(&override_path).expect_err("path suffix should fail");

    assert!(matches!(err, ConfigError::InvalidOverridePath { .. }));
    assert!(err.to_string().contains(".test.yml"));
}

#[test]
fn load_test_override_rejects_path_outside_sandbox_workspace() {
    let override_path = std::env::temp_dir().join(format!(
        "config-outside-{}-{}.test.yml",
        std::process::id(),
        unique_suffix()
    ));
    fs::write(&override_path, "version: 1\n").expect("write override");

    let err = load_test_override(&override_path).expect_err("outside path should fail");

    let _ = fs::remove_file(&override_path);
    assert!(matches!(err, ConfigError::InvalidOverridePath { .. }));
    assert!(err.to_string().contains("inside sandbox workspace"));
}

#[cfg(unix)]
#[test]
fn load_test_override_rejects_symlink_to_prd_baseline() {
    let root = test_workspace_dir("prd-symlink");
    fs::create_dir_all(&root).expect("create test dir");
    let link_path = root.join("prd-link.test.yml");
    let _ = fs::remove_file(&link_path);
    std::os::unix::fs::symlink(
        ConfigPath::prd().expect("resolve prd config").as_path(),
        &link_path,
    )
    .expect("create prd symlink");

    let err = load_test_override(&link_path).expect_err("prd symlink should fail");

    assert!(matches!(err, ConfigError::InvalidOverridePath { .. }));
    assert!(err.to_string().contains("prd.yml"));
}

#[test]
fn bench_template_round_trips_after_width_substitution() {
    // The bench driver substitutes the sweep width straight into
    // runtime.layerstack; a rendered arm file must load through every schema
    // section with no env side channel left in the template.
    let template = fs::read_to_string(workspace_root().join("config").join("bench.yml"))
        .expect("bench.yml is readable");
    assert!(
        !template.contains("EOS_"),
        "bench.yml must carry no env side channels"
    );
    let rendered = template
        .replace("__SWEEP_WIDTH__", "8")
        .replace("__DAEMON_CONFIG_PATH__", "/tmp/eos-bench-daemon.yml");
    let doc = parse_doc(&rendered);

    let runtime: configs::runtime::RuntimeConfig = doc.section("runtime").expect("runtime section");
    runtime.validate().expect("bench runtime config is valid");
    assert_eq!(runtime.layerstack.remount_sweep_width, 8);

    let daemon: configs::daemon::DaemonConfig = doc.section("daemon").expect("daemon section");
    daemon.validate().expect("bench daemon config is valid");

    let manager: configs::manager::ManagerConfig = doc.section("manager").expect("manager section");
    manager.validate().expect("bench manager config is valid");

    doc.section::<configs::observability::ObservabilityConfig>("observability")
        .expect("observability section");
}

fn test_workspace_dir(label: &str) -> PathBuf {
    workspace_root()
        .join("target")
        .join("config-tests")
        .join(format!(
            "{label}-{}-{}",
            std::process::id(),
            unique_suffix()
        ))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .find(|path| path.join("config").join("prd.yml").is_file())
        .expect("crate lives below sandbox workspace")
        .to_path_buf()
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_nanos()
}

fn parse_doc(text: &str) -> ConfigDocument {
    ConfigDocument::parse(Path::new("<test>"), text).expect("document parses")
}
