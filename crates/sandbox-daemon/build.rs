use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let jemalloc = env::var_os("CARGO_FEATURE_JEMALLOC").is_some();
    let backend = if jemalloc {
        manifest_dir.join("build/allocator_jemalloc.rs")
    } else {
        manifest_dir.join("build/allocator_system.rs")
    };
    let metrics = if jemalloc {
        manifest_dir.join("build/allocator_metrics_jemalloc.rs")
    } else {
        manifest_dir.join("build/allocator_metrics_system.rs")
    };
    println!(
        "cargo:rustc-env=SANDBOX_DAEMON_ALLOCATOR_BACKEND={}",
        backend.display()
    );
    println!(
        "cargo:rustc-env=SANDBOX_DAEMON_ALLOCATOR_METRICS={}",
        metrics.display()
    );
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_JEMALLOC");
    println!("cargo:rerun-if-changed=build/allocator_jemalloc.rs");
    println!("cargo:rerun-if-changed=build/allocator_metrics_jemalloc.rs");
    println!("cargo:rerun-if-changed=build/allocator_metrics_system.rs");
    println!("cargo:rerun-if-changed=build/allocator_system.rs");
    Ok(())
}
