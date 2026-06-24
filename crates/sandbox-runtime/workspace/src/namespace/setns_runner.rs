use std::path::PathBuf;

use sandbox_runtime_namespace_execution::NamespaceTarget;
use serde_json::json;

#[cfg(target_os = "linux")]
use crate::isolated_setup::{BRIDGE_PREFIX_LEN, GATEWAY};
use crate::lifecycle::remount::{RemountOverlayResult, RemountProbe};
use crate::model::WorkspaceHandle;
use crate::profile::WorkspaceModeError;
use crate::profile::WorkspaceModeHandle;

#[cfg(target_os = "linux")]
use super::fds::{expect_line, write_all_fd};
#[cfg(target_os = "linux")]
use super::holder::ns_holder_runtime_error;
use super::setup_error;
use super::NamespaceRuntime;

impl NamespaceRuntime {
    pub(crate) fn mount_overlay(
        &self,
        handle: &WorkspaceModeHandle,
        layer_paths: &[PathBuf],
    ) -> Result<(), WorkspaceModeError> {
        #[cfg(not(target_os = "linux"))]
        {
            #[cfg(feature = "test-support")]
            if self.force_engine_for_test {
                return self.mount_overlay_via_engine(handle, layer_paths);
            }
            let _ = (handle, layer_paths);
            Ok(())
        }
        #[cfg(target_os = "linux")]
        {
            self.mount_overlay_via_engine(handle, layer_paths)
        }
    }

    pub(crate) fn remount_overlay(
        &self,
        handle: &WorkspaceModeHandle,
        layer_paths: &[PathBuf],
        probe: &RemountProbe,
    ) -> Result<RemountOverlayResult, WorkspaceModeError> {
        #[cfg(not(target_os = "linux"))]
        {
            #[cfg(feature = "test-support")]
            if self.force_engine_for_test {
                return self.remount_overlay_via_engine(handle, layer_paths, probe);
            }
            let _ = (handle, layer_paths, probe);
            Ok(RemountOverlayResult::verified())
        }
        #[cfg(target_os = "linux")]
        {
            self.remount_overlay_via_engine(handle, layer_paths, probe)
        }
    }

    pub(crate) fn mount_overlay_via_engine(
        &self,
        handle: &WorkspaceModeHandle,
        layer_paths: &[PathBuf],
    ) -> Result<(), WorkspaceModeError> {
        let mut entry = WorkspaceHandle::from(handle).entry().map_err(setup_error)?;
        entry.layer_paths = layer_paths.to_vec();
        let id = self.engine.allocate_id();
        self.engine
            .run_mount(
                "--mount-overlay",
                NamespaceTarget::from(entry),
                id,
                json!({}),
                |_| Ok(()),
            )
            .map_err(setup_error)?
            .wait()
            .map_err(setup_error)
    }

    pub(crate) fn remount_overlay_via_engine(
        &self,
        handle: &WorkspaceModeHandle,
        layer_paths: &[PathBuf],
        probe: &RemountProbe,
    ) -> Result<RemountOverlayResult, WorkspaceModeError> {
        let mut entry = WorkspaceHandle::from(handle).entry().map_err(setup_error)?;
        entry.layer_paths = layer_paths.to_vec();
        let id = self.engine.allocate_id();
        let probe_args = json!({
            "probe_path": probe
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "probe_content": probe.expected_content.as_deref(),
        });
        self.engine
            .run_mount(
                "--remount-overlay",
                NamespaceTarget::from(entry),
                id,
                probe_args,
                |outcome| Ok(RemountOverlayResult::from_payload(outcome.payload())),
            )
            .map_err(setup_error)?
            .wait()
            .map_err(setup_error)
    }

    pub(crate) fn signal_net_ready(
        &self,
        handle: &WorkspaceModeHandle,
        setup_timeout_s: f64,
    ) -> Result<(), WorkspaceModeError> {
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
