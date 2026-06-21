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
daemon:
  commands:
    scratch_root: /eos/scratch/commands
runner:
  env:
    inherit_keys: [PATH, HOME]
"#,
    );
    let override_doc = parse_doc(
        r#"
daemon:
  commands:
    scratch_root: /tmp/eos/commands
runner:
  env:
    inherit_keys: [TZ]
"#,
    );

    baseline.merge(override_doc).expect("merge succeeds");

    let daemon = baseline
        .section::<Value>("daemon")
        .expect("daemon section deserializes");
    let runner = baseline
        .section::<Value>("runner")
        .expect("runner section deserializes");
    assert_eq!(
        daemon["commands"]["scratch_root"],
        Value::String("/tmp/eos/commands".to_owned())
    );
    assert_eq!(
        runner["env"]["inherit_keys"],
        Value::Sequence(vec![Value::String("TZ".to_owned())])
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
  env:
    inherit_keys: [PATH]
"#,
    )
    .expect("write override");

    let doc = load_test_override(&override_path).expect("override loads");
    let section = doc.section::<Value>("runner").expect("section loads");

    assert_eq!(
        section["env"]["inherit_keys"],
        Value::Sequence(vec![Value::String("PATH".to_owned())])
    );
    assert!(matches!(
        section["env"]["restricted_keys"],
        Value::Sequence(_)
    ));
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
