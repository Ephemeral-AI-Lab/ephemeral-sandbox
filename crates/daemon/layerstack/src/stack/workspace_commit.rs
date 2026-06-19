use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::error::LayerStackError;
use crate::fs::{
    clear_storage_root_preserving_lock_and_names, copy_path, fsync_dir, fsync_tree_files,
    next_unique, read_manifest, record_elapsed, remove_path, replace_workspace_contents,
    write_atomic,
};
use crate::lock::STORAGE_WRITER_LOCK_FILE;
use crate::model::Manifest;
use crate::workspace_base::build_workspace_base_from_snapshot;
use crate::ACTIVE_MANIFEST_FILE;

use super::leases::lock_shared_registry;
use super::{LayerStack, MergedView};

pub(crate) const COMMIT_WORKSPACE_JOURNAL_FILE: &str = "commit_to_workspace.json";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommitWorkspacePhase {
    Staged,
    ReplacingWorkspace { workspace_root: String },
    WorkspaceReplaced,
}

#[derive(Debug, Deserialize, Serialize)]
struct CommitWorkspaceJournal {
    phase: CommitWorkspacePhase,
    staged_storage_root: String,
}

pub(crate) fn commit_workspace_journal_path(storage_root: &Path) -> PathBuf {
    storage_root.join(COMMIT_WORKSPACE_JOURNAL_FILE)
}

pub(crate) fn write_commit_workspace_journal(
    storage_root: &Path,
    phase: CommitWorkspacePhase,
    staged_storage: &Path,
) -> Result<(), LayerStackError> {
    let journal = CommitWorkspaceJournal {
        phase,
        staged_storage_root: staged_storage.to_string_lossy().into_owned(),
    };
    let encoded = serde_json::to_vec_pretty(&journal)
        .map_err(|err| LayerStackError::Storage(err.to_string()))?;
    write_atomic(commit_workspace_journal_path(storage_root), &encoded)
}

pub(crate) fn recover_commit_to_workspace(storage_root: &Path) -> Result<(), LayerStackError> {
    let journal_path = commit_workspace_journal_path(storage_root);
    if !journal_path.exists() {
        return Ok(());
    }
    let journal = read_commit_workspace_journal(&journal_path)?;
    let staged_storage = validate_staged_storage_path(storage_root, &journal.staged_storage_root)?;
    match journal.phase {
        CommitWorkspacePhase::Staged => {
            remove_path(&staged_storage)?;
            remove_path(&journal_path)?;
            fsync_dir(storage_root)?;
            Ok(())
        }
        CommitWorkspacePhase::ReplacingWorkspace { workspace_root } => {
            let workspace_root = validate_workspace_root_path(&workspace_root)?;
            recover_workspace_replacement(storage_root, &staged_storage, &workspace_root)?;
            install_staged_workspace_commit(storage_root, &staged_storage)
        }
        CommitWorkspacePhase::WorkspaceReplaced => {
            install_staged_workspace_commit(storage_root, &staged_storage)
        }
    }
}

fn read_commit_workspace_journal(path: &Path) -> Result<CommitWorkspaceJournal, LayerStackError> {
    serde_json::from_str(&std::fs::read_to_string(path)?)
        .map_err(|err| LayerStackError::Storage(format!("read commit journal: {err}")))
}

fn recover_workspace_replacement(
    storage_root: &Path,
    staged_storage: &Path,
    workspace_root: &Path,
) -> Result<(), LayerStackError> {
    let active = read_manifest(staged_storage.join(ACTIVE_MANIFEST_FILE))?;
    let projection = allocate_commit_projection_dir(storage_root, "projected-recovery")?;
    let result = (|| {
        MergedView::new(staged_storage.to_path_buf()).project(&projection, &active)?;
        replace_workspace_contents(workspace_root, &projection)?;
        fsync_dir(workspace_root)?;
        Ok(())
    })();
    let _ = remove_path(&projection);
    result
}

pub(crate) fn allocate_commit_projection_dir(
    storage_root: &Path,
    prefix: &str,
) -> Result<PathBuf, LayerStackError> {
    let parent = storage_root.join("runtime").join("commit");
    std::fs::create_dir_all(&parent)?;
    for _ in 0..100 {
        let candidate = parent.join(format!("{prefix}-{}-{}", std::process::id(), next_unique()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(LayerStackError::Storage(format!(
        "could not allocate commit projection directory for prefix {prefix}"
    )))
}

fn validate_workspace_root_path(workspace_root: &str) -> Result<PathBuf, LayerStackError> {
    let path = PathBuf::from(workspace_root);
    if path.as_os_str().is_empty() {
        return Err(LayerStackError::Storage(
            "commit workspace path is empty".to_owned(),
        ));
    }
    if !path.is_absolute() {
        return Err(LayerStackError::Storage(format!(
            "commit workspace path must be absolute: {}",
            path.display()
        )));
    }
    Ok(path)
}

pub(crate) fn install_staged_workspace_commit(
    storage_root: &Path,
    staged_storage: &Path,
) -> Result<(), LayerStackError> {
    clear_storage_root_preserving_lock_and_names(storage_root, &[COMMIT_WORKSPACE_JOURNAL_FILE])?;
    for child in std::fs::read_dir(staged_storage)? {
        let child = child?;
        if child.file_name() == std::ffi::OsStr::new(STORAGE_WRITER_LOCK_FILE) {
            continue;
        }
        copy_path(&child.path(), &storage_root.join(child.file_name()))?;
    }
    fsync_tree_files(storage_root)?;
    fsync_dir(storage_root)?;
    remove_path(staged_storage)?;
    remove_path(&storage_root.join(COMMIT_WORKSPACE_JOURNAL_FILE))?;
    fsync_dir(storage_root)?;
    Ok(())
}

fn validate_staged_storage_path(
    storage_root: &Path,
    staged_storage_root: &str,
) -> Result<PathBuf, LayerStackError> {
    let path = PathBuf::from(staged_storage_root);
    let expected_parent = storage_root.parent().ok_or_else(|| {
        LayerStackError::Storage(format!(
            "storage root has no parent: {}",
            storage_root.display()
        ))
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            LayerStackError::Storage(format!(
                "staged commit storage path has no file name: {}",
                path.display()
            ))
        })?;
    if path.parent() != Some(expected_parent)
        || !file_name.starts_with(&staged_storage_name_prefix(storage_root))
    {
        return Err(LayerStackError::Storage(format!(
            "invalid staged commit storage path: {}",
            path.display()
        )));
    }
    Ok(path)
}

pub(crate) fn staged_storage_name_prefix(storage_root: &Path) -> String {
    let name = storage_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("layerstack");
    format!(".{name}.commit-storage-")
}

impl LayerStack {
    pub fn commit_to_workspace(
        &mut self,
        workspace_root: &Path,
    ) -> Result<(Manifest, BTreeMap<String, f64>), LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let total_start = Instant::now();
        if !workspace_root.is_dir() {
            return Err(LayerStackError::WorkspaceBinding(format!(
                "workspace_root does not exist: {}",
                workspace_root.display()
            )));
        }
        if lock_shared_registry(&self.leases)?.active_count() > 0 {
            return Err(LayerStackError::Storage(
                "commit_to_workspace blocked by active leases".to_owned(),
            ));
        }

        let active = self.read_active_manifest_unlocked()?;
        let projection = self.commit_projection_dir()?;
        let staged_storage = self.commit_staged_storage_dir()?;
        let mut timings = BTreeMap::new();
        let storage_root = self.storage_root.clone();
        let view = &mut self.view;
        let mut journal_requires_recovery = false;
        let outcome = (|| {
            let workspace_root_for_journal = workspace_root
                .canonicalize()
                .unwrap_or_else(|_| workspace_root.to_path_buf());
            let project_start = Instant::now();
            view.project(&projection, &active)?;
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.project_s",
                project_start,
            );

            let rebuild_start = Instant::now();
            let _ = build_workspace_base_from_snapshot(
                &staged_storage,
                &storage_root,
                workspace_root,
                &projection,
                false,
            )?;
            write_commit_workspace_journal(
                &storage_root,
                CommitWorkspacePhase::Staged,
                &staged_storage,
            )?;

            write_commit_workspace_journal(
                &storage_root,
                CommitWorkspacePhase::ReplacingWorkspace {
                    workspace_root: workspace_root_for_journal.to_string_lossy().into_owned(),
                },
                &staged_storage,
            )?;
            journal_requires_recovery = true;
            let replace_start = Instant::now();
            replace_workspace_contents(workspace_root, &projection)?;
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.replace_workspace_s",
                replace_start,
            );
            write_commit_workspace_journal(
                &storage_root,
                CommitWorkspacePhase::WorkspaceReplaced,
                &staged_storage,
            )?;
            journal_requires_recovery = true;

            install_staged_workspace_commit(&storage_root, &staged_storage)?;
            journal_requires_recovery = false;
            *view = MergedView::new(storage_root.clone());
            let new_manifest = read_manifest(storage_root.join(ACTIVE_MANIFEST_FILE))?;
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.rebuild_base_s",
                rebuild_start,
            );
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.total_s",
                total_start,
            );
            Ok(new_manifest)
        })();
        let _ = remove_path(&projection);
        if outcome.is_err() && !journal_requires_recovery {
            let _ = remove_path(&staged_storage);
            let _ = remove_path(&commit_workspace_journal_path(&storage_root));
        }
        outcome.map(|manifest| (manifest, timings))
    }

    fn commit_projection_dir(&self) -> Result<PathBuf, LayerStackError> {
        allocate_commit_projection_dir(&self.storage_root, "projected")
    }

    pub(crate) fn commit_staged_storage_dir(&self) -> Result<PathBuf, LayerStackError> {
        let parent = self.storage_root.parent().ok_or_else(|| {
            LayerStackError::Storage(format!(
                "storage root has no parent: {}",
                self.storage_root.display()
            ))
        })?;
        std::fs::create_dir_all(parent)?;
        let prefix = staged_storage_name_prefix(&self.storage_root);
        for _ in 0..100 {
            let candidate =
                parent.join(format!("{prefix}{}-{}", std::process::id(), next_unique()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.into()),
            }
        }
        Err(LayerStackError::Storage(
            "could not allocate staged commit storage directory".to_owned(),
        ))
    }
}
