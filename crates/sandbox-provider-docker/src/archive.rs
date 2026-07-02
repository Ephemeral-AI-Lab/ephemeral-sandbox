//! In-memory tar builder for uploading daemon assets via the Docker Engine
//! `put_archive` endpoint. Entries are rooted at `/`, so the archive is
//! extracted with `path = "/"`.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};

use bytes::Bytes;
use sandbox_runtime_layerstack::{
    WorkspaceBinding, ACTIVE_MANIFEST_FILE, LAYERS_DIR, LAYER_METADATA_DIR,
    MANIFEST_SCHEMA_VERSION, SHARED_BASE_DIR, STAGING_DIR, WORKSPACE_BASE_LAYER_ID,
    WORKSPACE_BINDING_FILE,
};
use serde_json::json;

const DAEMON_BINARY_MODE: u32 = 0o755;
const CONFIG_FILE_MODE: u32 = 0o644;
const DIRECTORY_MODE: u32 = 0o755;

/// Build a tar archive carrying the Linux daemon binary and config YAML at their
/// container paths, plus every parent directory entry they require.
pub fn build_install_archive(
    daemon_binary_container_path: &Path,
    daemon_binary: &[u8],
    config_container_path: &Path,
    config_yaml: &[u8],
) -> io::Result<Bytes> {
    let mut builder = tar::Builder::new(Vec::new());
    append_parent_dirs(&mut builder, daemon_binary_container_path)?;
    append_file(
        &mut builder,
        daemon_binary_container_path,
        daemon_binary,
        DAEMON_BINARY_MODE,
    )?;
    append_parent_dirs(&mut builder, config_container_path)?;
    append_file(
        &mut builder,
        config_container_path,
        config_yaml,
        CONFIG_FILE_MODE,
    )?;
    let inner = builder.into_inner()?;
    Ok(Bytes::from(inner))
}

pub fn build_shared_base_seed_archive(
    layer_stack_root: &Path,
    workspace_root: &Path,
    root_hash: &str,
) -> io::Result<Bytes> {
    let mut builder = tar::Builder::new(Vec::new());
    append_dir(&mut builder, workspace_root)?;
    append_dir(&mut builder, &layer_stack_root.join(LAYERS_DIR))?;
    append_dir(&mut builder, &layer_stack_root.join(STAGING_DIR))?;
    append_dir(&mut builder, &layer_stack_root.join(LAYER_METADATA_DIR))?;

    let manifest = json!({
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "version": 1,
        "layers": [{
            "layer_id": WORKSPACE_BASE_LAYER_ID,
            "path": format!("{SHARED_BASE_DIR}/{WORKSPACE_BASE_LAYER_ID}"),
        }],
    });
    let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(json_error)?;
    append_file(
        &mut builder,
        &layer_stack_root.join(ACTIVE_MANIFEST_FILE),
        &manifest_json,
        CONFIG_FILE_MODE,
    )?;

    let binding = WorkspaceBinding {
        workspace_root: workspace_root.to_string_lossy().into_owned(),
        layer_stack_root: layer_stack_root.to_string_lossy().into_owned(),
        base_root_hash: root_hash.to_owned(),
    };
    let binding_json = serde_json::to_vec_pretty(&binding).map_err(json_error)?;
    append_file(
        &mut builder,
        &layer_stack_root.join(WORKSPACE_BINDING_FILE),
        &binding_json,
        CONFIG_FILE_MODE,
    )?;

    append_file(
        &mut builder,
        &layer_stack_root
            .join(LAYER_METADATA_DIR)
            .join(format!("{WORKSPACE_BASE_LAYER_ID}.digest")),
        root_hash.as_bytes(),
        CONFIG_FILE_MODE,
    )?;

    let inner = builder.into_inner()?;
    Ok(Bytes::from(inner))
}

pub fn build_shared_base_volume_archive(
    volume_mount_root: &Path,
    shared_base_source: &Path,
) -> io::Result<Bytes> {
    let mut builder = tar::Builder::new(Vec::new());
    append_dir(&mut builder, volume_mount_root)?;
    append_host_tree(&mut builder, shared_base_source, volume_mount_root)?;
    let inner = builder.into_inner()?;
    Ok(Bytes::from(inner))
}

fn append_file(
    builder: &mut tar::Builder<Vec<u8>>,
    container_path: &Path,
    data: &[u8],
    mode: u32,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(data.len() as u64);
    header.set_mode(mode);
    builder.append_data(&mut header, tar_entry_path(container_path), data)
}

fn append_host_file(
    builder: &mut tar::Builder<Vec<u8>>,
    source_path: &Path,
    container_path: &Path,
    metadata: &std::fs::Metadata,
) -> io::Result<()> {
    let mut file = std::fs::File::open(source_path)?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(metadata.len());
    header.set_mode(metadata.permissions().mode() & 0o7777);
    builder.append_data(&mut header, tar_entry_path(container_path), &mut file)
}

fn append_dir(builder: &mut tar::Builder<Vec<u8>>, container_path: &Path) -> io::Result<()> {
    append_dir_with_mode(builder, container_path, DIRECTORY_MODE)
}

fn append_dir_with_mode(
    builder: &mut tar::Builder<Vec<u8>>,
    container_path: &Path,
    mode: u32,
) -> io::Result<()> {
    append_parent_dirs(builder, container_path)?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(mode);
    builder.append_data(&mut header, tar_entry_path(container_path), io::empty())
}

fn append_symlink(
    builder: &mut tar::Builder<Vec<u8>>,
    source_path: &Path,
    container_path: &Path,
) -> io::Result<()> {
    let link_target = std::fs::read_link(source_path)?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_link_name(link_target)?;
    builder.append_data(&mut header, tar_entry_path(container_path), io::empty())
}

fn append_host_tree(
    builder: &mut tar::Builder<Vec<u8>>,
    source_root: &Path,
    container_root: &Path,
) -> io::Result<()> {
    let mut entries = std::fs::read_dir(source_root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let container_path = container_root.join(entry.file_name());
        append_host_entry(builder, &source_path, &container_path)?;
    }
    Ok(())
}

fn append_host_entry(
    builder: &mut tar::Builder<Vec<u8>>,
    source_path: &Path,
    container_path: &Path,
) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(source_path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        append_symlink(builder, source_path, container_path)
    } else if file_type.is_dir() {
        append_dir_with_mode(
            builder,
            container_path,
            metadata.permissions().mode() & 0o7777,
        )?;
        append_host_tree(builder, source_path, container_path)
    } else if file_type.is_file() {
        append_host_file(builder, source_path, container_path, &metadata)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported shared base entry: {}", source_path.display()),
        ))
    }
}

fn append_parent_dirs(builder: &mut tar::Builder<Vec<u8>>, file_path: &Path) -> io::Result<()> {
    let Some(parent) = file_path.parent() else {
        return Ok(());
    };
    let mut accumulated = String::new();
    for component in parent.components() {
        if let Component::Normal(segment) = component {
            accumulated.push_str(&segment.to_string_lossy());
            accumulated.push('/');
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(DIRECTORY_MODE);
            builder.append_data(&mut header, &accumulated, io::empty())?;
        }
    }
    Ok(())
}

fn tar_entry_path(container_path: &Path) -> String {
    container_path
        .to_string_lossy()
        .trim_start_matches('/')
        .to_owned()
}

fn json_error(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn shared_base_volume_archive_materializes_shared_base_tree() {
        let root = temp_root();
        let source = root.join("source-base");
        let base = source.join(WORKSPACE_BASE_LAYER_ID);
        let bin = base.join("bin");
        fs::create_dir_all(&bin).expect("create source dirs");
        let tool = bin.join("tool.sh");
        fs::write(&tool, b"#!/bin/sh\n").expect("write tool");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod tool");
        std::os::unix::fs::symlink("tool.sh", bin.join("tool-link")).expect("symlink");

        let bytes = build_shared_base_volume_archive(Path::new("/seed-base"), &source)
            .expect("build archive");
        let out = root.join("out");
        fs::create_dir_all(&out).expect("create out");
        tar::Archive::new(bytes.as_ref())
            .unpack(&out)
            .expect("unpack archive");

        let extracted_tool = out.join("seed-base/B000001-base/bin/tool.sh");
        assert_eq!(
            fs::read(&extracted_tool).expect("read tool"),
            b"#!/bin/sh\n"
        );
        assert_eq!(
            fs::metadata(&extracted_tool)
                .expect("tool metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_link(out.join("seed-base/B000001-base/bin/tool-link")).expect("read link"),
            PathBuf::from("tool.sh")
        );

        let _ = fs::remove_dir_all(root);
    }

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "eos-docker-archive-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time after epoch")
                .as_nanos()
        ))
    }
}
