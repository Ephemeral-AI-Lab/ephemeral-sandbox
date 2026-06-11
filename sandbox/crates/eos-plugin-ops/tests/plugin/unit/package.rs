use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn copy_package_tree_preserves_nested_files_and_modes() -> TestResult {
    let root = unique_temp_dir("copy-package-tree");
    let staged = root.join("staged");
    let target = root.join("target");
    fs::create_dir_all(staged.join("runtime"))?;
    fs::write(staged.join("sandbox-plugin.json"), "{}")?;
    fs::write(staged.join("runtime/server.py"), "#!/usr/bin/env python3\n")?;
    fs::set_permissions(
        staged.join("runtime/server.py"),
        fs::Permissions::from_mode(0o755),
    )?;

    copy_package_tree(&staged, &target)?;

    assert_eq!(
        fs::read_to_string(target.join("sandbox-plugin.json"))?,
        "{}"
    );
    assert_eq!(
        fs::metadata(target.join("runtime/server.py"))?
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let _ = fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn publish_package_replaces_existing_root_without_leaving_temps() -> TestResult {
    let root = unique_temp_dir("publish-package");
    let staged = root.join("upload").join("package");
    let package_root = root.join("runtime").join("plugins").join("lsp").join("new");
    fs::create_dir_all(&staged)?;
    fs::create_dir_all(&package_root)?;
    fs::write(staged.join(PACKAGE_SHA256_MARKER), "new\n")?;
    fs::write(staged.join("server.js"), "new")?;
    fs::write(package_root.join(PACKAGE_SHA256_MARKER), "old\n")?;
    fs::write(package_root.join("stale.js"), "old")?;
    let paths = PackagePaths {
        package_root: package_root.clone(),
        dependency_root: root
            .join("runtime")
            .join("packages")
            .join("lsp")
            .join("new"),
        upload_digest_root: root.join("upload"),
        setup_tmp_root: root.join("setup"),
    };

    assert!(publish_package(&staged, &paths)?);

    assert_eq!(
        fs::read_to_string(package_root.join(PACKAGE_SHA256_MARKER))?,
        "new\n"
    );
    assert_eq!(fs::read_to_string(package_root.join("server.js"))?, "new");
    assert!(!package_root.join("stale.js").exists());
    let runtime_parent = package_root.parent().expect("package root has parent");
    let temp_entries = fs::read_dir(runtime_parent)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".new."))
        .count();
    assert_eq!(temp_entries, 0);
    let _ = fs::remove_dir_all(&root);
    Ok(())
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
}
