//! Host-neutral plugin route + service-process *specification* data.
//!
//! Pure DTOs: a [`PluginOperationRoute`] (how one public op dispatches) and a
//! [`PluginProcessSpec`] (what a service process launches — command, env,
//! package roots, PPC socket). The daemon owns the impure half over this data:
//! spawning the live process and shaping these into wire JSON.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eos_namespace::protocol::Intent;
use sha2::{Digest, Sha256};

use eos_plugin::{PluginError, PluginServiceKey, ServiceMode};

pub const ENV_PLUGIN_PPC_SOCKET: &str = "EOS_PLUGIN_PPC_SOCKET";
pub const ENV_PLUGIN_LAYER_STACK_ROOT: &str = "EOS_PLUGIN_LAYER_STACK_ROOT";
pub const ENV_PLUGIN_WORKSPACE_ROOT: &str = "EOS_PLUGIN_WORKSPACE_ROOT";
pub const ENV_PLUGIN_PACKAGE_ROOT: &str = "EOS_PLUGIN_PACKAGE_ROOT";
pub const ENV_PLUGIN_DEPENDENCY_ROOT: &str = "EOS_PLUGIN_DEPENDENCY_ROOT";
pub const ENV_PLUGIN_ID: &str = "EOS_PLUGIN_ID";
pub const ENV_PLUGIN_DIGEST: &str = "EOS_PLUGIN_DIGEST";
pub const ENV_PLUGIN_SERVICE_ID: &str = "EOS_PLUGIN_SERVICE_ID";
pub const ENV_PLUGIN_SERVICE_PROFILE_DIGEST: &str = "EOS_PLUGIN_SERVICE_PROFILE_DIGEST";
pub const ENV_PLUGIN_PPC_PROTOCOL_VERSION: &str = "EOS_PLUGIN_PPC_PROTOCOL_VERSION";
pub const ENV_PLUGIN_WORKSPACE_MOUNTED: &str = "EOS_PLUGIN_WORKSPACE_MOUNTED";

/// How one resolved public op dispatches: read-only service, overlay one-shot,
/// or self-managed write callback. Wire shaping (`to_json`) is daemon-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginOperationRoute {
    pub plugin_id: String,
    pub op_name: String,
    pub public_op: String,
    pub layer_stack_root: Option<String>,
    pub intent: Intent,
    pub auto_workspace_overlay: bool,
    pub service_id: Option<String>,
    pub service_instance_id: Option<String>,
    pub service_key: Option<PluginServiceKey>,
    pub service_mode: Option<ServiceMode>,
    pub service_command: Vec<String>,
    pub service_ppc_protocol_version: Option<u32>,
    pub timeout_ms: Option<u64>,
}

impl PluginOperationRoute {
    pub const fn dispatch_mode(&self) -> &'static str {
        match self.intent {
            Intent::ReadOnly => "read_only_service",
            Intent::WriteAllowed if self.auto_workspace_overlay => "write_allowed_oneshot_overlay",
            Intent::WriteAllowed => "self_managed_callback",
            Intent::Lifecycle => "invalid_lifecycle",
        }
    }
}

/// What a `WorkspaceSnapshotRefresh` service process launches. The daemon
/// spawns it (`adapters/plugins/process.rs`); this is the launch contract only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginProcessSpec {
    pub key: PluginServiceKey,
    pub command: Vec<String>,
    pub package_root: PathBuf,
    pub dependency_root: PathBuf,
    pub working_dir: PathBuf,
    pub ppc_protocol_version: u32,
    pub socket_path: PathBuf,
}

impl PluginProcessSpec {
    pub fn new_with_package_paths(
        key: PluginServiceKey,
        command: Vec<String>,
        package_root: PathBuf,
        dependency_root: PathBuf,
        working_dir: PathBuf,
        ppc_protocol_version: u32,
        socket_root: impl AsRef<Path>,
    ) -> Result<Self, PluginError> {
        if command.is_empty() || command[0].trim().is_empty() {
            return Err(PluginError::Manifest(format!(
                "service {} requires a launch command",
                key.service_id
            )));
        }
        if ppc_protocol_version == 0 {
            return Err(PluginError::Manifest(
                "ppc_protocol_version must be positive".to_owned(),
            ));
        }
        let socket_path = socket_path_for_key(&key, socket_root.as_ref());
        Ok(Self {
            key,
            command,
            package_root,
            dependency_root,
            working_dir,
            ppc_protocol_version,
            socket_path,
        })
    }

    pub fn environment(&self) -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            (
                ENV_PLUGIN_PPC_SOCKET,
                self.socket_path.to_string_lossy().into_owned(),
            ),
            (
                ENV_PLUGIN_LAYER_STACK_ROOT,
                self.key.layer_stack_root.clone(),
            ),
            (ENV_PLUGIN_WORKSPACE_ROOT, self.key.workspace_root.clone()),
            (
                ENV_PLUGIN_PACKAGE_ROOT,
                self.package_root.to_string_lossy().into_owned(),
            ),
            (
                ENV_PLUGIN_DEPENDENCY_ROOT,
                self.dependency_root.to_string_lossy().into_owned(),
            ),
            (ENV_PLUGIN_ID, self.key.plugin_id.clone()),
            (ENV_PLUGIN_DIGEST, self.key.plugin_digest.clone()),
            (ENV_PLUGIN_SERVICE_ID, self.key.service_id.clone()),
            (
                ENV_PLUGIN_SERVICE_PROFILE_DIGEST,
                self.key.service_profile_digest.clone(),
            ),
            (
                ENV_PLUGIN_PPC_PROTOCOL_VERSION,
                self.ppc_protocol_version.to_string(),
            ),
            (ENV_PLUGIN_WORKSPACE_MOUNTED, "0".to_owned()),
        ])
    }

    pub fn service_instance_id(&self) -> String {
        self.key.service_instance_id()
    }
}

fn socket_path_for_key(key: &PluginServiceKey, socket_root: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(key.service_instance_id().as_bytes());
    hasher.update(b"\0");
    hasher.update(key.plugin_digest.as_bytes());
    let digest = hasher.finalize();
    socket_root.join(format!("{}.sock", lower_hex(&digest[..16])))
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}
