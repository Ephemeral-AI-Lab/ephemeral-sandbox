use std::error::Error;
use std::path::PathBuf;

use sandbox_observability_telemetry::ObservabilityPaths;

#[test]
fn derives_one_log_and_its_rotated_sibling_from_daemon_socket_path() -> Result<(), Box<dyn Error>> {
    let daemon_runtime_dir = PathBuf::from("/eos/runtime/daemon");
    let socket_path = daemon_runtime_dir.join("runtime.sock");

    let paths = ObservabilityPaths::from_socket_path(&socket_path)?;

    assert_eq!(paths.daemon_runtime_dir(), daemon_runtime_dir);
    assert_eq!(
        paths.observability_dir(),
        daemon_runtime_dir.join("observability")
    );
    assert_eq!(
        paths.log_path(),
        daemon_runtime_dir
            .join("observability")
            .join("observability.ndjson")
    );
    assert_eq!(
        paths.rotated_log_path(),
        daemon_runtime_dir
            .join("observability")
            .join("observability.ndjson.1")
    );
    assert_eq!(
        paths.resource_log_path(),
        daemon_runtime_dir
            .join("observability")
            .join("resources.ndjson")
    );
    assert_eq!(
        paths.rotated_resource_log_path(),
        daemon_runtime_dir
            .join("observability")
            .join("resources.ndjson.1")
    );

    Ok(())
}
