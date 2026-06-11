use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eos_layerstack::{LayerRef, Manifest, MergedView, WorkspaceBinding, MANIFEST_SCHEMA_VERSION};
use serde_json::json;

use crate::direct::{api_error, parse_layer_path, resolve_layer_path, usize_to_f64_saturating};
use crate::{
    FileBackend, FileOpsError, Mutation, MutationKind, MutationOutcome, ReadBytes,
    ResolvedWorkspacePath, WorkspaceTimings,
};

#[derive(Debug, Clone)]
pub struct IsolatedBackend {
    pub layer_stack_root: PathBuf,
    pub workspace_root: PathBuf,
    pub upperdir: PathBuf,
    pub layer_paths: Vec<PathBuf>,
    pub manifest_version: i64,
    pub manifest_root_hash: String,
}

impl FileBackend for IsolatedBackend {
    fn workspace_kind(&self) -> &'static str {
        "isolated"
    }

    fn mutation_source(&self, _kind: MutationKind) -> &'static str {
        "isolated_workspace"
    }

    fn resolve_path(&self, request_path: &str) -> Result<ResolvedWorkspacePath, FileOpsError> {
        let binding = WorkspaceBinding {
            workspace_root: self.workspace_root.to_string_lossy().into_owned(),
            layer_stack_root: self.layer_stack_root.to_string_lossy().into_owned(),
            active_manifest_version: self.manifest_version,
            active_root_hash: self.manifest_root_hash.clone(),
            base_manifest_version: self.manifest_version,
            base_root_hash: self.manifest_root_hash.clone(),
        };
        resolve_layer_path(&binding, request_path)
    }

    fn read_bytes(&self, path: &ResolvedWorkspacePath) -> Result<ReadBytes, FileOpsError> {
        let read_start = std::time::Instant::now();
        let layer_path = parse_layer_path(&path.path)?;
        let (bytes, exists) = self.read_current(layer_path.as_str())?;
        let mut timings = self.timings(0);
        timings.insert(
            "api.read.layer_stack_read_s".to_owned(),
            json!(read_start.elapsed().as_secs_f64()),
        );
        Ok(ReadBytes {
            bytes,
            exists,
            manifest_version: Some(self.manifest_version),
            timings,
        })
    }

    fn apply(&self, mutation: Mutation) -> Result<MutationOutcome, FileOpsError> {
        let layer_path = parse_layer_path(&mutation.path.path)?;
        let target = self.upperdir.join(layer_path.as_str());
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(api_error)?;
        }
        std::fs::write(target, &mutation.content).map_err(api_error)?;
        let changed_paths = vec![layer_path.as_str().to_owned()];
        Ok(MutationOutcome {
            workspace_kind: "isolated".to_owned(),
            success: true,
            published: false,
            status: "committed".to_owned(),
            conflict: None,
            conflict_reason: None,
            changed_path_kinds: BTreeMap::from([(
                layer_path.as_str().to_owned(),
                "write".to_owned(),
            )]),
            changed_paths,
            mutation_source: self.mutation_source(mutation.kind).to_owned(),
            timings: self.timings(1),
            ..MutationOutcome::default()
        })
    }
}

impl IsolatedBackend {
    fn read_current(&self, layer_path: &str) -> Result<(Option<Vec<u8>>, bool), FileOpsError> {
        let upper_path = self.upperdir.join(layer_path);
        match std::fs::symlink_metadata(&upper_path) {
            Ok(metadata) if metadata.is_file() => {
                return Ok((Some(std::fs::read(upper_path).map_err(api_error)?), true));
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Ok((
                    Some(
                        std::fs::read_link(upper_path)
                            .map_err(api_error)?
                            .to_string_lossy()
                            .as_bytes()
                            .to_vec(),
                    ),
                    true,
                ));
            }
            Ok(_) => return Ok((None, false)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(api_error(error)),
        }
        MergedView::new(self.layer_stack_root.clone())
            .read_bytes(layer_path, &self.snapshot_manifest())
            .map_err(api_error)
    }

    fn snapshot_manifest(&self) -> Manifest {
        Manifest {
            version: self.manifest_version,
            schema_version: MANIFEST_SCHEMA_VERSION,
            layers: self
                .layer_paths
                .iter()
                .enumerate()
                .map(|(index, path)| LayerRef {
                    layer_id: format!("isolated-{index}"),
                    path: relative_layer_path(&self.layer_stack_root, path),
                })
                .collect(),
        }
    }

    fn timings(&self, changed_path_count: usize) -> WorkspaceTimings {
        BTreeMap::from([(
            "resource.command_exec.changed_path_count".to_owned(),
            json!(usize_to_f64_saturating(changed_path_count)),
        )])
    }
}

fn relative_layer_path(layer_stack_root: &Path, path: &Path) -> String {
    path.strip_prefix(layer_stack_root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}
