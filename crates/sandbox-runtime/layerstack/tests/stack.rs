use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_runtime_layerstack::MANIFEST_SCHEMA_VERSION;
use sandbox_runtime_layerstack::{
    build_workspace_base, ensure_workspace_base, LayerChange, LayerPath, LayerStack,
    LayerStackError, MergedView, WorkspaceBinding, ACTIVE_MANIFEST_FILE, WORKSPACE_BINDING_FILE,
};
use serde_json::json;

#[test]
fn delete_layer_hides_files_in_reads_and_projection(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("delete_hides");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "dir/a.txt", "one\n")?;
    publish_text(&mut stack, "dir/b.txt", "two\n")?;

    stack.publish_layer(&[LayerChange::Delete {
        path: LayerPath::parse("dir/a.txt")?,
    }])?;

    assert_eq!(stack.read_text("dir/a.txt")?, (String::new(), false));
    assert_eq!(stack.read_text("dir/b.txt")?, ("two\n".to_owned(), true));

    let manifest = stack.read_active_manifest()?;
    MergedView::new(fixture.root.clone()).project(&fixture.workspace, &manifest)?;
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
fn read_bytes_limited_rejects_oversized_file(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("read_bytes_limited");
    let mut stack = LayerStack::open(fixture.root.clone())?;
    publish_text(&mut stack, "large.txt", "abcdef")?;

    let error = stack
        .read_bytes_limited("large.txt", 2)
        .expect_err("oversized merged file read is rejected");

    assert!(
        matches!(error, LayerStackError::FileTooLarge { size: 6, limit: 2 }),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn ensure_workspace_base_rejects_too_new_manifest_schema(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("workspace_base_new_schema");
    write_bound_manifest(
        &fixture,
        json!({
            "schema_version": MANIFEST_SCHEMA_VERSION + 1,
            "version": 1,
            "layers": [{"layer_id": "L000001", "path": "layers/L000001"}],
        }),
    )?;

    let Err(err) = ensure_workspace_base(&fixture.root, &fixture.workspace) else {
        return Err("too-new manifest schema was accepted".into());
    };
    assert!(
        err.to_string().contains("schema_version"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn ensure_workspace_base_rejects_invalid_manifest_layer_paths(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

        let Err(err) = ensure_workspace_base(&fixture.root, &fixture.workspace) else {
            return Err(format!("{label} was accepted").into());
        };
        assert!(
            err.to_string().contains("layer path"),
            "{label} returned unexpected error: {err}"
        );
    }
    Ok(())
}

#[test]
fn build_workspace_base_writes_manifest_with_canonical_atomic_path(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

fn publish_text(
    stack: &mut LayerStack,
    path: &str,
    content: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    stack.publish_layer(&[LayerChange::Write {
        path: LayerPath::parse(path)?,
        content: content.as_bytes().to_vec(),
    }])?;
    Ok(())
}

fn write_bound_manifest(
    fixture: &Fixture,
    manifest: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all(&fixture.root)?;
    std::fs::create_dir_all(&fixture.workspace)?;
    let binding = WorkspaceBinding {
        workspace_root: fixture.workspace.to_string_lossy().into_owned(),
        layer_stack_root: fixture.root.to_string_lossy().into_owned(),
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
            "layerstack-{label}-{}-{}",
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
