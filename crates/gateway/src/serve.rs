use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use host::{HostConfig, SandboxHost};

use crate::transport;

pub(crate) fn run(argv: impl Iterator<Item = String>) -> Result<()> {
    let config = ServeArgs::parse(argv)?;
    let host = SandboxHost::open(config.host)?;
    transport::serve(&config.listen, Arc::new(host))
}

struct ServeArgs {
    listen: PathBuf,
    host: HostConfig,
}

impl ServeArgs {
    fn parse(mut argv: impl Iterator<Item = String>) -> Result<Self> {
        // The gateway and eosd are built from the same sandbox workspace, so
        // daemon config and packaged binary defaults are derivable here.
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .map(std::path::Path::to_path_buf)
            .context("derive workspace root")?;
        let default_runtime_dir = default_runtime_dir();
        let mut listen = default_runtime_dir.join("gateway.sock");
        let mut image = None;
        let mut platform = None;
        let mut docker_privileged = true;
        let mut eosd_path = workspace.join("dist").join("eosd-linux-amd64");
        let mut config_yaml_path = workspace.join("config").join("prd.yml");
        let mut remote_config_path = None;
        let mut remote_daemon_dir = PathBuf::from("/eos/runtime/daemon");
        let mut state_dir: Option<PathBuf> = None;
        let mut tcp_port = 37_657_u16;
        let mut ready_timeout_s = 60_u64;
        let mut request_timeout_s = 30_u64;
        let mut created_by = "sandbox-gateway".to_owned();
        while let Some(flag) = argv.next() {
            let mut value = || -> Result<String> {
                argv.next()
                    .with_context(|| format!("{flag} requires a value"))
            };
            match flag.as_str() {
                "--listen" => listen = value()?.into(),
                "--image" => image = Some(value()?),
                "--platform" => platform = Some(value()?),
                "--docker-privileged" => docker_privileged = true,
                "--no-docker-privileged" => docker_privileged = false,
                "--eosd" => eosd_path = value()?.into(),
                "--config-yaml" => config_yaml_path = value()?.into(),
                "--remote-config" => remote_config_path = Some(value()?.into()),
                "--remote-daemon-dir" => remote_daemon_dir = value()?.into(),
                "--state-dir" => state_dir = Some(value()?.into()),
                "--tcp-port" => tcp_port = value()?.parse().context("--tcp-port")?,
                "--ready-timeout-s" => {
                    ready_timeout_s = value()?.parse().context("--ready-timeout-s")?;
                }
                "--request-timeout-s" => {
                    request_timeout_s = value()?.parse().context("--request-timeout-s")?;
                }
                "--created-by" => created_by = value()?,
                other => bail!("unknown serve flag {other:?}"),
            }
        }
        let image = image.context("serve requires --image <docker image>")?;
        let state_dir = state_dir.unwrap_or_else(|| default_runtime_dir.join("state"));
        let remote_eosd_path = remote_daemon_dir.join("eosd");
        let remote_config_path =
            remote_config_path.unwrap_or_else(|| remote_daemon_dir.join("config.yml"));
        Ok(Self {
            listen,
            host: HostConfig {
                image,
                platform,
                docker_privileged,
                eosd_path,
                config_yaml_path,
                remote_daemon_dir,
                remote_eosd_path,
                remote_config_path,
                tcp_port,
                ready_timeout: Duration::from_secs(ready_timeout_s),
                request_timeout: Duration::from_secs(request_timeout_s),
                created_by,
                state_dir,
            },
        })
    }
}

fn default_runtime_dir() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("eos-sandbox");
    }
    let suffix = std::env::var("UID")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| std::process::id().to_string());
    std::env::temp_dir().join(format!("eos-sandbox-gateway-{suffix}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::ServeArgs;

    fn parse(args: &[&str]) -> ServeArgs {
        ServeArgs::parse(args.iter().map(|arg| (*arg).to_owned())).expect("serve args parse")
    }

    #[test]
    fn default_remote_config_lives_under_remote_daemon_dir() {
        let args = parse(&[
            "--image",
            "sandbox:latest",
            "--remote-daemon-dir",
            "/eos/custom/daemon",
        ]);

        assert_eq!(
            args.host.remote_config_path,
            PathBuf::from("/eos/custom/daemon/config.yml")
        );
    }

    #[test]
    fn explicit_remote_config_overrides_default() {
        let args = parse(&[
            "--image",
            "sandbox:latest",
            "--remote-daemon-dir",
            "/eos/custom/daemon",
            "--remote-config",
            "/eos/config/prd.yml",
        ]);

        assert_eq!(
            args.host.remote_config_path,
            PathBuf::from("/eos/config/prd.yml")
        );
    }

    #[test]
    fn parse_keeps_local_and_remote_config_paths_distinct() {
        let parsed = parse(&[
            "--image",
            "sandbox:dev",
            "--config-yaml",
            "/tmp/source.yml",
            "--remote-config",
            "/eos/runtime/config/prd.yml",
            "--listen",
            "/tmp/sandbox.sock",
        ]);

        assert_eq!(
            parsed.host.config_yaml_path,
            PathBuf::from("/tmp/source.yml")
        );
        assert_eq!(
            parsed.host.remote_config_path,
            PathBuf::from("/eos/runtime/config/prd.yml")
        );
    }

    #[test]
    fn docker_privileged_can_be_disabled_and_reenabled() {
        let disabled = parse(&["--image", "sandbox:dev", "--no-docker-privileged"]);
        assert!(!disabled.host.docker_privileged);

        let reenabled = parse(&[
            "--image",
            "sandbox:dev",
            "--no-docker-privileged",
            "--docker-privileged",
        ]);
        assert!(reenabled.host.docker_privileged);
    }

    #[test]
    fn defaults_use_private_runtime_dir_for_sockets_and_state() {
        let parsed = parse(&["--image", "sandbox:dev"]);

        assert_eq!(
            parsed.listen.file_name().and_then(|name| name.to_str()),
            Some("gateway.sock")
        );
        assert_eq!(
            parsed
                .host
                .state_dir
                .file_name()
                .and_then(|name| name.to_str()),
            Some("state")
        );
        assert_eq!(parsed.listen.parent(), parsed.host.state_dir.parent());
        assert_ne!(
            parsed.listen.parent(),
            Some(std::path::Path::new("/tmp")),
            "default operator socket must not be a direct /tmp sibling"
        );
    }
}
