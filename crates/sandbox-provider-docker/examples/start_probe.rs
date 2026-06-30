//! Throwaway probe: reproduce the manager's create+start via the workspace's
//! bollard to capture the exact error (Debug, not just Display).
//! Run: cargo run -p sandbox-provider-docker --example start_probe

use std::collections::HashMap;

use bollard::container::{Config, CreateContainerOptions, StartContainerOptions};
use bollard::models::{HostConfig, HostConfigCgroupnsModeEnum, PortBinding};
use bollard::Docker;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let docker = Docker::connect_with_local_defaults().expect("connect");
    let name = format!("eos-probe-{}", std::process::id());

    let mut exposed = HashMap::new();
    exposed.insert("7000/tcp".to_owned(), HashMap::new());
    exposed.insert("7001/tcp".to_owned(), HashMap::new());
    let mut bindings = HashMap::new();
    bindings.insert(
        "7000/tcp".to_owned(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_owned()),
            host_port: None,
        }]),
    );
    bindings.insert(
        "7001/tcp".to_owned(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_owned()),
            host_port: None,
        }]),
    );

    let host_config = HostConfig {
        privileged: Some(true),
        cgroupns_mode: Some(HostConfigCgroupnsModeEnum::PRIVATE),
        init: Some(true),
        port_bindings: Some(bindings),
        ..Default::default()
    };
    let config = Config {
        image: Some("eos-e2e-readme:latest".to_owned()),
        cmd: Some(vec!["sleep".to_owned(), "10".to_owned()]),
        exposed_ports: Some(exposed),
        host_config: Some(host_config),
        ..Default::default()
    };

    let create = docker
        .create_container(
            Some(CreateContainerOptions {
                name: name.clone(),
                platform: None,
            }),
            config,
        )
        .await;
    println!("create: {create:?}");

    let start = docker
        .start_container(&name, None::<StartContainerOptions<String>>)
        .await;
    println!("start: {start:?}");

    let inspect = docker.inspect_container(&name, None).await;
    println!(
        "state: {:?}",
        inspect.as_ref().ok().and_then(|c| c.state.clone())
    );

    let _ = docker
        .remove_container(
            &name,
            Some(bollard::container::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
    println!("cleaned up");
}
