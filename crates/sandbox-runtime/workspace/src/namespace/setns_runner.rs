use std::path::PathBuf;

#[cfg(target_os = "linux")]
use crate::isolated_network_setup::{BRIDGE_PREFIX_LEN, GATEWAY};
#[cfg(target_os = "linux")]
use crate::model::WorkspaceHandle;
use crate::session::MountedWorkspace;
use crate::session::WorkspaceManagerError;
#[cfg(target_os = "linux")]
use sandbox_observability::record::names;
#[cfg(target_os = "linux")]
use sandbox_runtime_namespace_execution::NamespaceTarget;

#[cfg(target_os = "linux")]
use super::fds::{expect_line, write_all_fd};
#[cfg(target_os = "linux")]
use super::holder::ns_holder_runtime_error;
#[cfg(target_os = "linux")]
use super::setup_error;
use super::NamespaceRuntime;

impl NamespaceRuntime {
    pub(crate) fn mount_overlay(
        &self,
        handle: &MountedWorkspace,
        layer_paths: &[PathBuf],
    ) -> Result<(), WorkspaceManagerError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (&self.engine, &self.obs, handle, layer_paths);
            Ok(())
        }
        #[cfg(target_os = "linux")]
        {
            let mut entry = WorkspaceHandle::from(handle).entry().map_err(setup_error)?;
            entry.layer_paths = layer_paths.to_vec();
            let id = self.engine.allocate_id();
            let mount = self
                .engine
                .mount_overlay(NamespaceTarget::from(entry), id)
                .map_err(setup_error)?;
            self.obs
                .scope(names::NAMESPACE_EXEC_MOUNT_OVERLAY, |_span| {
                    mount.wait().map_err(setup_error)
                })
        }
    }

    pub(crate) fn signal_net_ready(
        &self,
        handle: &MountedWorkspace,
        setup_timeout_s: f64,
    ) -> Result<(), WorkspaceManagerError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (handle, setup_timeout_s);
        }
        #[cfg(target_os = "linux")]
        {
            let payload = handle.veth.as_ref().map_or_else(
                || "net-ready\n".to_owned(),
                |veth| {
                    format!(
                        "net-ready {} {} {} {}\n",
                        veth.ns_name, veth.ns_ip, BRIDGE_PREFIX_LEN, GATEWAY
                    )
                },
            );
            write_all_fd(handle.control_fd, payload.as_bytes())?;
            if let Err(error) = expect_line(handle.readiness_fd, b"ready", setup_timeout_s) {
                return Err(ns_holder_runtime_error(error, handle.holder_pid)?);
            }
        }
        Ok(())
    }
}
