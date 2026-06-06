use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use eos_layerstack::{
    build_workspace_base, ensure_workspace_base, LayerChange, LayerPath, LayerRef, LayerStack,
    WorkspaceBinding, ACTIVE_MANIFEST_FILE, WORKSPACE_BINDING_FILE,
};
use eos_protocol::MANIFEST_SCHEMA_VERSION;
use serde_json::json;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn squash_coalesces_layers_and_preserves_merged_reads() -> TestResult {
    let fixture = Fixture::new("squash_basic");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "a.txt", "one\n")?;
    publish_text(&mut stack, "b.txt", "two\n")?;
    publish_text(&mut stack, "a.txt", "three\n")?;

    assert!(stack.can_squash(2)?);
    let squashed = stack
        .squash(2)?
        .ok_or_else(|| std::io::Error::other("squash should produce a manifest"))?;

    assert_eq!(squashed.layers.len(), 1);
    assert_eq!(stack.read_text("a.txt")?.0, "three\n");
    assert_eq!(stack.read_text("b.txt")?.0, "two\n");
    assert!(stack.squash(2)?.is_none());
    Ok(())
}

#[test]
fn release_lease_gcs_squashed_layers_after_retaining_lease_drops() -> TestResult {
    let fixture = Fixture::new("squash_gc");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "a.txt", "one\n")?;
    publish_text(&mut stack, "b.txt", "two\n")?;
    publish_text(&mut stack, "c.txt", "three\n")?;

    let lease = stack.acquire_snapshot("reader")?;
    let old_tail: Vec<LayerRef> = lease.manifest.layers[1..].to_vec();
    let squashed = stack
        .squash(2)?
        .ok_or_else(|| std::io::Error::other("squash should produce a manifest"))?;
    assert_eq!(squashed.layers.len(), 2);
    for layer in &old_tail {
        assert!(fixture.root.join(&layer.path).exists());
    }

    assert!(stack.release_lease(&lease.lease_id)?);
    for layer in &old_tail {
        assert!(!fixture.root.join(&layer.path).exists());
    }
    Ok(())
}

#[test]
fn cross_instance_lease_retains_squashed_layers_until_reopened_release() -> TestResult {
    let fixture = Fixture::new("squash_gc_cross_instance");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "a.txt", "one\n")?;
    publish_text(&mut stack, "b.txt", "two\n")?;
    publish_text(&mut stack, "c.txt", "three\n")?;
    drop(stack);

    let lease_stack = LayerStack::open(fixture.root.clone())?;
    let lease = lease_stack.acquire_snapshot("reader")?;
    let old_tail: Vec<LayerRef> = lease.manifest.layers[1..].to_vec();
    assert_eq!(
        LayerStack::open(fixture.root.clone())?.active_lease_count(),
        1
    );

    let mut squash_stack = LayerStack::open(fixture.root.clone())?;
    let squashed = squash_stack
        .squash(2)?
        .ok_or_else(|| std::io::Error::other("squash should produce a manifest"))?;
    assert_eq!(squashed.layers.len(), 2);
    for layer in &old_tail {
        assert!(fixture.root.join(&layer.path).exists());
    }

    assert!(LayerStack::open(fixture.root.clone())?.release_lease(&lease.lease_id)?);
    assert_eq!(
        LayerStack::open(fixture.root.clone())?.active_lease_count(),
        0
    );
    for layer in &old_tail {
        assert!(!fixture.root.join(&layer.path).exists());
    }
    Ok(())
}

#[test]
fn delete_layer_hides_files_in_reads_and_projection() -> TestResult {
    let fixture = Fixture::new("delete_hides");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "dir/a.txt", "one\n")?;
    publish_text(&mut stack, "dir/b.txt", "two\n")?;

    stack.publish_layer(&[LayerChange::Delete {
        path: LayerPath::parse("dir/a.txt")?,
    }])?;

    assert_eq!(stack.read_text("dir/a.txt")?, (String::new(), false));
    assert_eq!(stack.read_text("dir/b.txt")?, ("two\n".to_owned(), true));

    std::fs::create_dir_all(&fixture.workspace)?;
    let _ = stack.commit_to_workspace(&fixture.workspace)?;
    assert!(!fixture.workspace.join("dir/a.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("dir/b.txt"))?,
        "two\n"
    );
    assert!(
        !fixture.workspace.join("dir/.wh.a.txt").exists(),
        "logical whiteout marker must not leak into projections"
    );
    Ok(())
}

#[test]
fn commit_to_workspace_projects_active_manifest_and_rebuilds_base() -> TestResult {
    let fixture = Fixture::new("commit_workspace");
    std::fs::create_dir_all(fixture.workspace.join(".git"))?;
    std::fs::write(fixture.workspace.join(".git/config"), "[core]\n")?;
    std::fs::write(fixture.workspace.join("tracked.txt"), "base\n")?;
    build_workspace_base(&fixture.root, &fixture.workspace, false)?;

    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "tracked.txt", "overlay\n")?;
    publish_text(&mut stack, "new.txt", "new\n")?;

    let (manifest, timings) = stack.commit_to_workspace(&fixture.workspace)?;

    assert_eq!(manifest.version, 1);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("tracked.txt"))?,
        "overlay\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("new.txt"))?,
        "new\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join(".git/config"))?,
        "[core]\n"
    );
    assert!(timings.contains_key("layer_stack.commit_to_workspace.project_s"));
    assert!(timings.contains_key("layer_stack.commit_to_workspace.replace_workspace_s"));
    assert!(timings.contains_key("layer_stack.commit_to_workspace.rebuild_base_s"));
    assert!(timings.contains_key("layer_stack.commit_to_workspace.total_s"));
    assert_eq!(stack.read_text("tracked.txt")?.0, "overlay\n");
    Ok(())
}

#[test]
fn ensure_workspace_base_rejects_too_new_manifest_schema() -> TestResult {
    let fixture = Fixture::new("workspace_base_new_schema");
    write_bound_manifest(
        &fixture,
        json!({
            "schema_version": MANIFEST_SCHEMA_VERSION + 1,
            "version": 1,
            "layers": [{"layer_id": "L000001", "path": "layers/L000001"}],
        }),
    )?;

    let err = match ensure_workspace_base(&fixture.root, &fixture.workspace) {
        Ok(_) => return Err("too-new manifest schema was accepted".into()),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("schema_version"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn ensure_workspace_base_rejects_invalid_manifest_layer_paths() -> TestResult {
    let cases = [
        ("workspace_base_empty_layer_path", ""),
        ("workspace_base_parent_layer_path", "../outside"),
        ("workspace_base_absolute_layer_path", "/abs/layer"),
        ("workspace_base_nul_layer_path", "layers/\0bad"),
    ];
    for (label, path) in cases {
        let fixture = Fixture::new(label);
        write_bound_manifest(
            &fixture,
            json!({
                "schema_version": MANIFEST_SCHEMA_VERSION,
                "version": 1,
                "layers": [{"layer_id": "L000001", "path": path}],
            }),
        )?;

        let err = match ensure_workspace_base(&fixture.root, &fixture.workspace) {
            Ok(_) => return Err(format!("{label} was accepted").into()),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("layer path"),
            "{label} returned unexpected error: {err}"
        );
    }
    Ok(())
}

#[test]
fn build_workspace_base_writes_manifest_with_canonical_atomic_path() -> TestResult {
    let fixture = Fixture::new("workspace_base_manifest_atomic");
    std::fs::create_dir_all(&fixture.workspace)?;
    std::fs::write(fixture.workspace.join("tracked.txt"), "base\n")?;

    build_workspace_base(&fixture.root, &fixture.workspace, false)?;

    let manifest = fixture.root.join(ACTIVE_MANIFEST_FILE);
    assert!(manifest.exists());
    let manifest_payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest)?)?;
    assert_eq!(
        manifest_payload["schema_version"].as_i64(),
        Some(MANIFEST_SCHEMA_VERSION)
    );
    let stale_tmp = std::fs::read_dir(&fixture.root)?.try_fold(false, |found, entry| {
        let entry = entry?;
        Ok::<_, std::io::Error>(
            found
                || entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".manifest.json."),
        )
    })?;
    assert!(!stale_tmp, "atomic manifest writer left a temporary file");
    Ok(())
}

fn publish_text(stack: &mut LayerStack, path: &str, content: &str) -> TestResult {
    stack.publish_layer(&[LayerChange::Write {
        path: LayerPath::parse(path)?,
        content: content.as_bytes().to_vec(),
    }])?;
    Ok(())
}

fn write_bound_manifest(fixture: &Fixture, manifest: serde_json::Value) -> TestResult {
    std::fs::create_dir_all(&fixture.root)?;
    std::fs::create_dir_all(&fixture.workspace)?;
    let binding = WorkspaceBinding {
        workspace_root: fixture.workspace.to_string_lossy().into_owned(),
        layer_stack_root: fixture.root.to_string_lossy().into_owned(),
        active_manifest_version: 1,
        active_root_hash: "root".to_owned(),
        base_manifest_version: 1,
        base_root_hash: "root".to_owned(),
    };
    std::fs::write(
        fixture.root.join(WORKSPACE_BINDING_FILE),
        serde_json::to_vec_pretty(&binding)?,
    )?;
    std::fs::write(
        fixture.root.join(ACTIVE_MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "eos-layerstack-{label}-{}-{}",
            std::process::id(),
            NEXT_TMP_WRITE.fetch_add(1, Ordering::Relaxed)
        ));
        let workspace = root.with_extension("workspace");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&workspace);
        Self { root, workspace }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

static NEXT_TMP_WRITE: AtomicU64 = AtomicU64::new(0);
