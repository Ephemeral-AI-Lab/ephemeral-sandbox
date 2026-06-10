//! Mount-namespace masking for model-facing shell commands.
//!
//! The workspace overlay is mounted only at `workspace_root`, but the shell still
//! runs in the surrounding container root. Mask daemon/kernel control paths after
//! overlay setup so model-facing commands cannot inspect the real runtime state.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::RunnerError;

pub(crate) fn mask_model_shell_paths(hidden_paths: &[PathBuf]) -> Result<(), RunnerError> {
    for path in hidden_paths {
        mask_with_empty_tmpfs(path)?;
    }
    Ok(())
}

fn mask_with_empty_tmpfs(path: &Path) -> Result<(), RunnerError> {
    if !path.exists() {
        return Ok(());
    }
    if !path.is_dir() {
        return Err(RunnerError::InvalidRequest(format!(
            "masked path is not a directory: {}",
            path.display()
        )));
    }

    let source = CString::new("tmpfs").expect("static string has no nul");
    let target = CString::new(path.as_os_str().as_bytes()).map_err(|err| {
        RunnerError::InvalidRequest(format!("masked path contains an interior nul byte: {err}"))
    })?;
    let fstype = CString::new("tmpfs").expect("static string has no nul");
    let data = CString::new("size=4k,mode=000").expect("static string has no nul");
    let flags = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC | libc::MS_RDONLY;

    // SAFETY: `target`, `source`, `fstype`, and `data` are NUL-terminated strings
    // that live for the syscall. This function runs only after the runner is in
    // the dedicated mount namespace where it has CAP_SYS_ADMIN.
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags,
            data.as_ptr().cast(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(RunnerError::Syscall(std::io::Error::last_os_error()))
    }
}
