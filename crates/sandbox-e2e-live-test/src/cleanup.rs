use std::cell::Cell;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use crate::cli_client::{CliClient, CLI_BIN};
use crate::config::CleanupPolicy;

/// Outcome of one teardown pass (§7.3); embedded into `summary.cleanup`. Errors
/// are human-readable and non-fatal (e.g. a destroy of an already-gone sandbox).
#[derive(Serialize, Clone)]
pub struct CleanupReport {
    pub policy: String,
    pub removed_run_root: bool,
    pub destroyed_sandbox_ids: Vec<String>,
    pub errors: Vec<String>,
}

/// RAII run guard owning this run's teardown: survivor-sandbox sweep keyed on
/// `reports/*/` dir names, gateway detach (attach-only ⇒ the gateway is never
/// stopped), then policy-gated `remove_dir_all(run_root)`. `Drop` performs the
/// teardown if it has not already run, so a panicking aggregate still tears
/// down. It does not run on `SIGKILL`/hard abort (§7.4, no-orphan-reaper limit).
pub struct RunGuard {
    run_root: PathBuf,
    cli: CliClient,
    policy: CleanupPolicy,
    run_succeeded: bool,
    done: Cell<bool>,
}

impl RunGuard {
    #[must_use]
    pub fn new(run_root: PathBuf, gateway_socket: PathBuf, policy: CleanupPolicy) -> Self {
        let cli = CliClient::new(PathBuf::from(CLI_BIN), gateway_socket);
        Self {
            run_root,
            cli,
            policy,
            run_succeeded: false,
            done: Cell::new(false),
        }
    }

    pub fn set_succeeded(&mut self, ok: bool) {
        self.run_succeeded = ok;
    }

    /// Non-destructive preview used to write `summary.cleanup` before any removal
    /// (§7.3): the ids that would be swept and whether the run root would be
    /// removed, with no destroy errors observed yet.
    #[must_use]
    pub fn plan(&self) -> CleanupReport {
        CleanupReport {
            policy: self.policy.as_str().to_owned(),
            removed_run_root: self.should_remove(),
            destroyed_sandbox_ids: self.captured_ids(),
            errors: Vec::new(),
        }
    }

    /// Execute teardown once: sweep survivor sandboxes, leave the attached
    /// gateway untouched, then (policy-gated) remove the run root. Idempotent —
    /// a destroy of an already-gone (or unspawnable) sandbox is recorded but
    /// non-fatal. The `done` guard is set only after the destructive steps
    /// complete, so a teardown interrupted by a panic can still be finished by
    /// `Drop`.
    pub fn teardown(&self) -> CleanupReport {
        let destroyed_sandbox_ids = self.captured_ids();
        let mut errors = Vec::new();
        for id in &destroyed_sandbox_ids {
            let record = self.cli.manager("destroy_sandbox", &["--sandbox-id", id]);
            if record.exit_code != 0 {
                errors.push(format!("destroy_sandbox {id}: {}", record.stderr.trim()));
            }
        }

        let mut removed_run_root = false;
        if self.should_remove() {
            match fs::remove_dir_all(&self.run_root) {
                Ok(()) => removed_run_root = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    removed_run_root = true;
                }
                Err(error) => {
                    errors.push(format!(
                        "remove_dir_all {}: {error}",
                        self.run_root.display()
                    ));
                }
            }
        }

        self.done.set(true);
        CleanupReport {
            policy: self.policy.as_str().to_owned(),
            removed_run_root,
            destroyed_sandbox_ids,
            errors,
        }
    }

    fn should_remove(&self) -> bool {
        match self.policy {
            CleanupPolicy::Always => true,
            CleanupPolicy::OnSuccess => self.run_succeeded,
            CleanupPolicy::Never => false,
        }
    }

    fn captured_ids(&self) -> Vec<String> {
        let reports = self.run_root.join("reports");
        let Ok(read_dir) = fs::read_dir(&reports) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = read_dir
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        ids.sort();
        ids
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if !self.done.get() {
            let _ = self.teardown();
        }
    }
}
