//! Setns mode: join the ns-holder's pre-opened namespaces, then spawn the tool.
//!
//! For each isolated-workspace call the runner `setns`es this single-threaded
//! caller into the holder's FDs in the order `user → mnt → pid → net`
//! (PID setns affects descendants only, so it precedes `fork`), optionally joins
//! the iws cgroup before spawning, then the child execs the command through the
//! same shell/tool primitive used by fresh-namespace mode. A
//! separate helper does the in-namespace overlay mount (`setns` into `user`+`mnt`,
//! then call [`eos_overlay::mount_overlay`]).
//!
//! `setns(2)` is the only raw syscall here; child creation stays behind
//! [`std::process::Command`] in `fresh_ns::execute_tool`. `#![deny(unsafe_op_in_unsafe_fn)]`
//! still forces a `// SAFETY:` note on every FFI block.

#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(any(test, target_os = "linux"))]
use std::os::fd::RawFd;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(any(test, target_os = "linux"))]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
use eos_overlay::OverlayHandle;

use super::error::RunnerError;
#[cfg(any(test, target_os = "linux"))]
use eos_cas::NsFds;
use eos_cas::{RunRequest, RunResult};

#[cfg(target_os = "linux")]
const RESOLV_CONF: &str = "/etc/resolv.conf";

/// `setns` into the held namespaces, then run the tool command.
///
/// # Safety
///
/// Calls `setns(2)` (which requires this to be the only thread in the process),
/// then delegates child spawning to the shared shell/tool primitive. The setns
/// FD order (`user`, `mnt`, `pid`, `net`) is load-bearing.
///
/// # Errors
///
/// Returns [`RunnerError`] when namespace FDs are missing, `setns`/cgroup join
/// fails, request validation fails, or child execution fails.
#[cfg(target_os = "linux")]
pub(crate) fn run_setns(request: &RunRequest) -> Result<RunResult, RunnerError> {
    //   setns(user), setns(mnt), setns(pid), setns(net) in order; join cgroup.procs
    //   before fork; pipe stdin_b64 to the child; fork → execvp(argv); waitpid and
    //   map waitstatus → exit code. The group is its own session so cancel killpgs it.
    let ns_fds = require_ns_fds(request)?;
    join_cgroup(request)?;
    join_namespaces(&ns_fds)?;
    super::fresh_ns::execute_tool(request, 0.0, Instant::now(), None)
}

#[cfg(not(target_os = "linux"))]
/// Return the non-Linux unsupported error for setns execution.
///
/// # Errors
///
/// Always returns [`RunnerError::Unsupported`] outside Linux because `setns(2)`
/// is unavailable.
pub(crate) fn run_setns(_request: &RunRequest) -> Result<RunResult, RunnerError> {
    Err(RunnerError::Unsupported)
}

/// Mount the overlay inside an existing workspace mount namespace.
///
/// The runner `setns`es into the holder's `user` then `mnt` FDs, gaining
/// `CAP_SYS_ADMIN` in that namespace before calling [`eos_overlay::mount_overlay`].
///
/// # Invariant
///
/// Calls `setns(2)` twice (`user`, then `mnt`) before the mount; must run on a
/// single-threaded caller until both setns calls complete.
///
/// # Errors
///
/// Returns [`RunnerError`] when required namespace/overlay paths are missing,
/// `setns` fails, or the overlay mount fails.
#[cfg(target_os = "linux")]
pub fn setns_overlay_mount(
    request: &RunRequest,
    config: &super::config::RunnerConfig,
) -> Result<(), RunnerError> {
    //   setns(ns_fds.user, CLONE_NEWUSER); setns(ns_fds.mnt, CLONE_NEWNS); then build
    //   OverlayHandle (newest-first lowerdirs + upper/work) and mount the overlay.
    let ns_fds = require_ns_fds(request)?;
    let user = ns_fds.user.ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires user ns fd".to_owned())
    })?;
    let mnt = ns_fds.mnt.ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires mnt ns fd".to_owned())
    })?;
    setns_fd("user", user.0, libc::CLONE_NEWUSER)?;
    setns_fd("mnt", mnt.0, libc::CLONE_NEWNS)?;
    let upperdir = request.upperdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires upperdir".to_owned())
    })?;
    let workdir = request.workdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires workdir".to_owned())
    })?;
    let handle = OverlayHandle {
        layer_paths: overlay_layer_paths(request),
        upperdir: upperdir.clone(),
        workdir: workdir.clone(),
    };
    let guard = eos_overlay::mount_overlay(&request.workspace_root.0, &handle)?;
    super::mount_mask::mask_model_shell_paths(&config.mount_mask.hidden_paths)?;
    // The setns mount helper is a one-shot process. The mounted overlay must
    // outlive this helper and remain pinned by the target mount namespace until
    // isolated teardown, matching the Rust helper that exits after mounting.
    std::mem::forget(guard);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
/// Return the non-Linux unsupported error for setns overlay mounting.
///
/// # Errors
///
/// Always returns [`RunnerError::Unsupported`] outside Linux because `setns(2)`
/// is unavailable.
pub fn setns_overlay_mount(
    _request: &RunRequest,
    _config: &super::config::RunnerConfig,
) -> Result<(), RunnerError> {
    Err(RunnerError::Unsupported)
}

/// Configure `/etc/resolv.conf` inside an existing workspace mount namespace.
///
/// The helper only applies the fallback when the current first nameserver is a
/// loopback resolver such as systemd-resolved's `127.0.0.53` stub.
///
/// # Errors
///
/// Returns [`RunnerError`] when namespace FDs are missing, `setns` fails, the
/// request lacks `fallback_dns`, or the private bind mount cannot be applied.
#[cfg(target_os = "linux")]
pub fn configure_dns(request: &RunRequest) -> Result<serde_json::Value, RunnerError> {
    let ns_fds = require_ns_fds(request)?;
    let user = ns_fds.user.ok_or_else(|| {
        RunnerError::InvalidRequest("configure_dns requires user ns fd".to_owned())
    })?;
    let mnt = ns_fds.mnt.ok_or_else(|| {
        RunnerError::InvalidRequest("configure_dns requires mnt ns fd".to_owned())
    })?;
    let fallback_dns = request
        .tool_call
        .args
        .get("fallback_dns")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            RunnerError::InvalidRequest("configure_dns requires fallback_dns".to_owned())
        })?;

    setns_fd("user", user.0, libc::CLONE_NEWUSER)?;
    setns_fd("mnt", mnt.0, libc::CLONE_NEWNS)?;

    let content = match fs::read_to_string(RESOLV_CONF) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({
                "applied_fallback": false,
                "previous_first_nameserver": null,
            }));
        }
        Err(err) => return Err(err.into()),
    };
    let previous = first_nameserver(&content).map(str::to_owned);
    let applied = previous.as_deref().is_some_and(needs_fallback_dns);
    if applied {
        bind_mount_resolv_conf(fallback_dns)?;
    }
    Ok(serde_json::json!({
        "applied_fallback": applied,
        "previous_first_nameserver": previous,
    }))
}

#[cfg(not(target_os = "linux"))]
/// Return the non-Linux unsupported error for DNS configuration.
///
/// # Errors
///
/// Always returns [`RunnerError::Unsupported`] outside Linux because `setns(2)`
/// and bind mounts are unavailable.
pub const fn configure_dns(_request: &RunRequest) -> Result<serde_json::Value, RunnerError> {
    Err(RunnerError::Unsupported)
}

#[cfg(any(test, target_os = "linux"))]
fn require_ns_fds(request: &RunRequest) -> Result<NsFds, RunnerError> {
    request
        .ns_fds
        .ok_or_else(|| RunnerError::InvalidRequest("setns mode requires ns_fds".to_owned()))
}

#[cfg(all(test, target_os = "linux"))]
fn namespace_fd_order(ns_fds: &NsFds) -> Vec<(&'static str, RawFd)> {
    namespace_fd_order_with_types(ns_fds)
        .into_iter()
        .map(|(name, fd, _)| (name, fd))
        .collect()
}

#[cfg(all(test, not(target_os = "linux")))]
fn namespace_fd_order(ns_fds: &NsFds) -> Vec<(&'static str, RawFd)> {
    [
        ("user", ns_fds.user),
        ("mnt", ns_fds.mnt),
        ("pid", ns_fds.pid),
        ("net", ns_fds.net),
    ]
    .into_iter()
    .filter_map(|(name, fd)| fd.map(|fd| (name, fd.0)))
    .collect()
}

#[cfg(target_os = "linux")]
fn namespace_fd_order_with_types(ns_fds: &NsFds) -> Vec<(&'static str, RawFd, libc::c_int)> {
    [
        ("user", ns_fds.user, libc::CLONE_NEWUSER),
        ("mnt", ns_fds.mnt, libc::CLONE_NEWNS),
        ("pid", ns_fds.pid, libc::CLONE_NEWPID),
        ("net", ns_fds.net, libc::CLONE_NEWNET),
    ]
    .into_iter()
    .filter_map(|(name, fd, nstype)| fd.map(|fd| (name, fd.0, nstype)))
    .collect()
}

#[cfg(any(test, target_os = "linux"))]
fn overlay_layer_paths(request: &RunRequest) -> Vec<PathBuf> {
    if request.layer_paths.is_empty() {
        vec![request.workspace_root.0.clone()]
    } else {
        request.layer_paths.clone()
    }
}

#[cfg(any(test, target_os = "linux"))]
fn first_nameserver(content: &str) -> Option<&str> {
    content.lines().find_map(|line| {
        let stripped = line.trim();
        stripped
            .strip_prefix("nameserver")
            .and_then(|rest| rest.split_whitespace().next())
    })
}

#[cfg(any(test, target_os = "linux"))]
fn needs_fallback_dns(addr: &str) -> bool {
    addr.starts_with("127.")
}

#[cfg(target_os = "linux")]
fn bind_mount_resolv_conf(fallback_dns: &str) -> Result<(), RunnerError> {
    let path = std::env::temp_dir().join(format!(
        ".iws-resolv-{}-{}.conf",
        std::process::id(),
        unique_suffix()
    ));
    fs::write(&path, format!("nameserver {fallback_dns}\n"))?;
    let source = cstring_path(&path)?;
    let target = CString::new(RESOLV_CONF)
        .map_err(|err| RunnerError::InvalidRequest(format!("invalid resolv.conf path: {err}")))?;
    let fstype = CString::new("none")
        .map_err(|err| RunnerError::InvalidRequest(format!("invalid mount fstype: {err}")))?;
    // SAFETY: after `setns(user,mnt)` this dedicated single-threaded helper has
    // CAP_SYS_ADMIN in the target namespace. `source`, `target`, and `fstype`
    // are NUL-terminated C strings that live for the call, and the data pointer
    // is null because MS_BIND ignores filesystem-specific data.
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    if rc == 0 {
        return Ok(());
    }
    Err(RunnerError::Syscall(std::io::Error::last_os_error()))
}

#[cfg(target_os = "linux")]
fn cstring_path(path: &std::path::Path) -> Result<CString, RunnerError> {
    CString::new(path.as_os_str().as_bytes()).map_err(|err| {
        RunnerError::InvalidRequest(format!(
            "path contains an interior nul byte: {} ({err})",
            path.display()
        ))
    })
}

#[cfg(target_os = "linux")]
fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(target_os = "linux")]
fn join_cgroup(request: &RunRequest) -> Result<(), RunnerError> {
    let Some(cgroup_path) = request.cgroup_path.as_ref() else {
        return Ok(());
    };
    let procs = cgroup_path.join("cgroup.procs");
    fs::write(procs, format!("{}\n", std::process::id())).map_err(RunnerError::Syscall)
}

#[cfg(target_os = "linux")]
fn join_namespaces(ns_fds: &NsFds) -> Result<(), RunnerError> {
    for (name, fd, nstype) in namespace_fd_order_with_types(ns_fds) {
        setns_fd(name, fd, nstype)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn setns_fd(name: &str, fd: RawFd, nstype: libc::c_int) -> Result<(), RunnerError> {
    // SAFETY: `fd` is a borrowed namespace file descriptor supplied by the
    // daemon to this dedicated single-threaded runner process. `nstype` is the
    // matching CLONE_NEW* constant for that descriptor, and no Rust references
    // are invalidated by the kernel changing the current task's namespace.
    let rc = unsafe { libc::setns(fd, nstype) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    let kind = err.kind();
    Err(RunnerError::Syscall(std::io::Error::new(
        kind,
        format!("setns({name}, fd={fd}, nstype=0x{nstype:x}) failed: {err}"),
    )))
}

#[cfg(test)]
mod tests {
    use super::{
        first_nameserver, namespace_fd_order, needs_fallback_dns, overlay_layer_paths,
        require_ns_fds,
    };
    use eos_cas::Intent;
    use eos_cas::{Fd, NsFds, RunMode, RunRequest, RunnerVerb, ToolCall, WorkspaceRoot};
    use std::path::Path;

    #[test]
    fn require_ns_fds_rejects_missing_setns_payload() -> Result<(), Box<dyn std::error::Error>> {
        let Err(error) = require_ns_fds(&request(None)) else {
            return Err("ns_fds should be required".into());
        };
        assert!(error.to_string().contains("requires ns_fds"));
        Ok(())
    }

    #[test]
    fn namespace_order_matches_rust_helper_and_skips_missing_fds() {
        let ns_fds = NsFds {
            user: Some(Fd(10)),
            mnt: Some(Fd(11)),
            pid: None,
            net: Some(Fd(12)),
        };
        assert_eq!(
            namespace_fd_order(&ns_fds),
            vec![("user", 10), ("mnt", 11), ("net", 12)]
        );
    }

    #[test]
    fn overlay_layer_paths_fall_back_to_workspace_root() {
        let request = request(Some(default_ns_fds()));
        assert_eq!(
            overlay_layer_paths(&request),
            vec![Path::new("/workspace").to_path_buf()]
        );
    }

    #[test]
    fn dns_fallback_detection_matches_rust_helper() {
        let content = "search local\nnameserver 127.0.0.53\nnameserver 8.8.8.8\n";
        let nameserver = first_nameserver(content);
        assert_eq!(nameserver, Some("127.0.0.53"));
        assert!(needs_fallback_dns(nameserver.unwrap_or_default()));
        assert!(!needs_fallback_dns("10.244.0.1"));
        assert_eq!(first_nameserver("search local\n"), None);
    }

    fn request(ns_fds: Option<NsFds>) -> RunRequest {
        RunRequest {
            mode: RunMode::SetNs,
            tool_call: ToolCall {
                invocation_id: "test".to_owned(),
                caller_id: "caller".to_owned(),
                verb: RunnerVerb::ExecCommand,
                intent: Intent::WriteAllowed,
                args: serde_json::json!({"command": "true"}),
                background: false,
            },
            workspace_root: WorkspaceRoot(Path::new("/workspace").to_path_buf()),
            layer_paths: vec![],
            upperdir: Some(Path::new("/tmp/iws/upper").to_path_buf()),
            workdir: Some(Path::new("/tmp/iws/work").to_path_buf()),
            ns_fds,
            cgroup_path: None,
            timeout_seconds: None,
        }
    }

    fn default_ns_fds() -> NsFds {
        NsFds {
            user: Some(Fd(10)),
            mnt: Some(Fd(11)),
            pid: Some(Fd(12)),
            net: Some(Fd(13)),
        }
    }
}
