//! Checkpoint commit pipeline: pathspec policy, worktree preparation
//! (overlay-or-projection), and the git staging/commit subprocess pipeline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use crate::response::usize_to_f64_saturating;
use eos_layerstack::{LayerStack, MergedView, WorkspaceBinding};
use eos_overlay::{
    allocate_overlay_writable_dirs, mount_overlay, overlay_writable_root, OverlayError,
    OverlayHandle, OverlayMount,
};

use super::{CheckpointError, CommitOutcome, CommitRequest};

/// Run the checkpoint commit pipeline for a single request.
///
/// Acquires a snapshot lease, projects (or overlay-mounts) the snapshot into a
/// throwaway worktree, stages the requested paths, and commits when the index
/// differs from `HEAD`. The lease is always released, and the worktree is torn
/// down by [`PreparedWorktree`]'s `Drop`.
pub fn commit_to_git(request: &CommitRequest<'_>) -> Result<CommitOutcome, CheckpointError> {
    let total_start = Instant::now();
    let root = request.layer_stack_root;
    let workspace_root = request.workspace_root;
    let mut stack = LayerStack::open(root.to_path_buf())?;
    let binding = eos_layerstack::require_workspace_binding(root)?;
    ensure_bound_workspace(&binding, workspace_root)?;
    let paths = normalize_paths(&request.raw_paths, &binding)?;
    let git_dir = resolve_git_dir(workspace_root)?;

    let lease_owner = format!("commit_to_git:{}", uuid::Uuid::new_v4().simple());
    let lease = stack.acquire_snapshot(&lease_owner)?;
    let manifest_version = lease.manifest_version;
    let manifest_root_hash = lease.root_hash.clone();
    let manifest_depth = lease.manifest.depth();
    let manifest_path_count = lease.layer_paths.len();
    let lease_id = lease.lease_id.clone();
    let mut timings = lease.timings.clone();

    let outcome = (|| {
        let worktree = prepare_worktree(root, &lease, &mut timings)?;
        let git_add_start = Instant::now();
        git_add(&git_dir, worktree.path(), &paths)?;
        record_elapsed(&mut timings, "api.commit_to_git.git_add_s", git_add_start);

        let diff_start = Instant::now();
        let has_changes = git_index_has_changes(&git_dir, worktree.path())?;
        record_elapsed(
            &mut timings,
            "api.commit_to_git.git_diff_cached_s",
            diff_start,
        );
        if !has_changes {
            let commit_sha = current_head(&git_dir, worktree.path())?;
            return Ok(Committed {
                committed: false,
                commit_sha,
                worktree_mode: worktree.mode(),
            });
        }

        let commit_start = Instant::now();
        git_commit(&git_dir, worktree.path(), request.message)?;
        record_elapsed(&mut timings, "api.commit_to_git.git_commit_s", commit_start);
        let commit_sha = current_head(&git_dir, worktree.path())?;
        Ok(Committed {
            committed: true,
            commit_sha,
            worktree_mode: worktree.mode(),
        })
    })();

    let release = stack.release_lease(&lease_id);
    match (outcome, release) {
        (Ok(committed), Ok(_)) => {
            timings.insert(
                "resource.layer_stack.manifest_depth".to_owned(),
                usize_to_f64_saturating(manifest_depth),
            );
            timings.insert(
                "resource.layer_stack.manifest_path_count".to_owned(),
                usize_to_f64_saturating(manifest_path_count),
            );
            record_elapsed(&mut timings, "api.commit_to_git.total_s", total_start);
            Ok(CommitOutcome {
                committed: committed.committed,
                commit_sha: committed.commit_sha,
                manifest_version,
                manifest_root_hash,
                paths,
                worktree_mode: committed.worktree_mode,
                timings,
            })
        }
        (Err(err), Ok(_)) => Err(err),
        (Ok(_), Err(err)) | (Err(_), Err(err)) => Err(err.into()),
    }
}

/// The git-pipeline result, carried out from the lease-scoped closure before the
/// worktree is dropped.
struct Committed {
    committed: bool,
    commit_sha: Option<String>,
    worktree_mode: &'static str,
}

fn ensure_bound_workspace(
    binding: &WorkspaceBinding,
    workspace_root: &Path,
) -> Result<(), CheckpointError> {
    let bound = Path::new(&binding.workspace_root);
    if bound != workspace_root {
        return Err(CheckpointError::InvalidEnvelope(format!(
            "workspace_root must match LayerStack binding: expected {}, got {}",
            bound.display(),
            workspace_root.display()
        )));
    }
    Ok(())
}

fn normalize_paths(
    raw_paths: &[String],
    binding: &WorkspaceBinding,
) -> Result<Vec<String>, CheckpointError> {
    raw_paths
        .iter()
        .filter_map(|raw| match normalize_pathspec(raw, binding) {
            Ok(Some(path)) => Some(Ok(path)),
            Ok(None) => None,
            Err(err) => Some(Err(err)),
        })
        .collect()
}

fn normalize_pathspec(
    raw: &str,
    binding: &WorkspaceBinding,
) -> Result<Option<String>, CheckpointError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(None);
    }
    let path = if trimmed.starts_with('/') {
        binding.layer_path_from_absolute(trimmed)?
    } else {
        binding.layer_path_from_relative(trimmed)?
    };
    if path == ".git" || path.starts_with(".git/") {
        return Err(CheckpointError::Forbidden(
            "commit_to_git cannot stage .git paths".to_owned(),
        ));
    }
    Ok(Some(path))
}

struct PreparedWorktree {
    path: PathBuf,
    mode: &'static str,
    mount: Option<OverlayMount>,
    run_dir: PathBuf,
}

impl PreparedWorktree {
    fn path(&self) -> &Path {
        &self.path
    }

    const fn mode(&self) -> &'static str {
        self.mode
    }
}

impl Drop for PreparedWorktree {
    fn drop(&mut self) {
        drop(self.mount.take());
        let _ = std::fs::remove_dir_all(&self.run_dir);
    }
}

fn prepare_worktree(
    root: &Path,
    lease: &eos_layerstack::Lease,
    timings: &mut BTreeMap<String, f64>,
) -> Result<PreparedWorktree, CheckpointError> {
    if let Some(worktree) = try_prepare_overlay_worktree(lease, timings)? {
        return Ok(worktree);
    }
    prepare_projected_worktree(root, lease, timings)
}

fn try_prepare_overlay_worktree(
    lease: &eos_layerstack::Lease,
    timings: &mut BTreeMap<String, f64>,
) -> Result<Option<PreparedWorktree>, CheckpointError> {
    let writable_root = match overlay_writable_root() {
        Ok(root) => root,
        Err(OverlayError::WritableRootUnavailable(_)) | Err(OverlayError::Unsupported) => {
            return Ok(None);
        }
        Err(err) => return Err(overlay_error("prepare overlay writable root", err)),
    };
    let run_dir = writable_root
        .join("commit-to-git")
        .join(uuid::Uuid::new_v4().simple().to_string());
    std::fs::create_dir_all(&run_dir)?;
    let dirs = allocate_overlay_writable_dirs(&run_dir)
        .map_err(|err| overlay_error("allocate commit_to_git overlay dirs", err))?;
    let mountpoint = run_dir.join("worktree");
    std::fs::create_dir_all(&mountpoint)?;
    let mount_start = Instant::now();
    let mount = match mount_overlay(
        &mountpoint,
        &OverlayHandle {
            upperdir: dirs.upperdir,
            workdir: dirs.workdir,
            layer_paths: lease.layer_paths.iter().map(PathBuf::from).collect(),
        },
    ) {
        Ok(mount) => mount,
        Err(OverlayError::Unsupported) => {
            let _ = std::fs::remove_dir_all(&run_dir);
            return Ok(None);
        }
        Err(err) => {
            let _ = std::fs::remove_dir_all(&run_dir);
            return Err(overlay_error("mount commit_to_git worktree", err));
        }
    };
    record_elapsed(timings, "api.commit_to_git.overlay_mount_s", mount_start);
    Ok(Some(PreparedWorktree {
        path: mountpoint,
        mode: "overlay",
        mount: Some(mount),
        run_dir,
    }))
}

fn prepare_projected_worktree(
    root: &Path,
    lease: &eos_layerstack::Lease,
    timings: &mut BTreeMap<String, f64>,
) -> Result<PreparedWorktree, CheckpointError> {
    let run_dir = std::env::temp_dir().join(format!(
        "eos-commit-to-git-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let worktree = run_dir.join("worktree");
    let project_start = Instant::now();
    MergedView::new(root.to_path_buf()).project(&worktree, &lease.manifest)?;
    record_elapsed(
        timings,
        "api.commit_to_git.project_worktree_s",
        project_start,
    );
    Ok(PreparedWorktree {
        path: worktree,
        mode: "projection",
        mount: None,
        run_dir,
    })
}

fn resolve_git_dir(workspace_root: &Path) -> Result<PathBuf, CheckpointError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("-c")
        .arg("safe.directory=*")
        .args(["rev-parse", "--absolute-git-dir"])
        .output()?;
    if !output.status.success() {
        return Err(CheckpointError::InvalidEnvelope(format!(
            "workspace_root must be a git repository: {}",
            command_stderr(&output)
        )));
    }
    let path = command_stdout(&output);
    if path.is_empty() {
        return Err(CheckpointError::InvalidEnvelope(
            "git rev-parse returned an empty git dir".to_owned(),
        ));
    }
    Ok(PathBuf::from(path))
}

fn git_add(git_dir: &Path, worktree: &Path, paths: &[String]) -> Result<(), CheckpointError> {
    let mut args = vec!["add", "-A", "--"];
    if paths.is_empty() {
        args.push(".");
    } else {
        args.extend(paths.iter().map(String::as_str));
    }
    run_git_checked(git_dir, worktree, &args).map(|_| ())
}

fn git_index_has_changes(git_dir: &Path, worktree: &Path) -> Result<bool, CheckpointError> {
    let output = run_git(
        git_dir,
        worktree,
        &["diff", "--cached", "--quiet", "--exit-code"],
    )?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(git_error("git diff --cached", &output)),
    }
}

fn git_commit(git_dir: &Path, worktree: &Path, message: &str) -> Result<(), CheckpointError> {
    run_git_checked(git_dir, worktree, &["commit", "-m", message]).map(|_| ())
}

fn current_head(git_dir: &Path, worktree: &Path) -> Result<Option<String>, CheckpointError> {
    let output = run_git(git_dir, worktree, &["rev-parse", "--verify", "HEAD"])?;
    if output.status.success() {
        return Ok(Some(command_stdout(&output)));
    }
    Ok(None)
}

fn run_git_checked(
    git_dir: &Path,
    worktree: &Path,
    args: &[&str],
) -> Result<Output, CheckpointError> {
    let output = run_git(git_dir, worktree, args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(git_error(&format!("git {}", args.join(" ")), &output))
    }
}

fn run_git(git_dir: &Path, worktree: &Path, args: &[&str]) -> Result<Output, CheckpointError> {
    Ok(Command::new("git")
        .arg("-c")
        .arg("safe.directory=*")
        .env("GIT_DIR", git_dir)
        .env("GIT_WORK_TREE", worktree)
        .env("GIT_AUTHOR_NAME", "EphemeralOS")
        .env("GIT_AUTHOR_EMAIL", "ephemeralos@example.invalid")
        .env("GIT_COMMITTER_NAME", "EphemeralOS")
        .env("GIT_COMMITTER_EMAIL", "ephemeralos@example.invalid")
        .args(args)
        .output()?)
}

fn git_error(command: &str, output: &Output) -> CheckpointError {
    CheckpointError::OverlayPipeline(format!(
        "{command} failed with status {}: {}",
        output.status,
        command_stderr(output)
    ))
}

fn overlay_error(context: &str, error: OverlayError) -> CheckpointError {
    CheckpointError::OverlayPipeline(format!("{context}: {error}"))
}

fn command_stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn command_stderr(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        stderr
    }
}

fn record_elapsed(timings: &mut BTreeMap<String, f64>, key: &str, start: Instant) {
    timings.insert(key.to_owned(), start.elapsed().as_secs_f64());
}
