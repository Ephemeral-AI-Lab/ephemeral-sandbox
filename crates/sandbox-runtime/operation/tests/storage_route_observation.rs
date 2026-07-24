mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_observability_telemetry::Observer;
use sandbox_runtime::{
    LayerStackService, LayerstackRuntimeConfig, StorageAuthority, StorageRolloutMode,
};

struct Fixture {
    base: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "storage-route-observation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace)?;
        sandbox_runtime_layerstack::build_workspace_base(&root, &workspace, false)?;
        Ok(Self { base })
    }

    fn service(&self) -> Result<LayerStackService, Box<dyn std::error::Error + Send + Sync>> {
        Ok(LayerStackService::new(
            self.base.join("layer-stack"),
            self.base.join("scratch"),
            LayerstackRuntimeConfig {
                rollout_mode: StorageRolloutMode::Legacy,
                ..LayerstackRuntimeConfig::default()
            },
            Observer::disabled(),
            support::test_file_service(),
        )?)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn operation_observation_maps_the_legacy_only_route(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new()?;
    let observation = fixture.service()?.observe()?;

    assert_eq!(observation.route.schema_version, 1);
    assert_eq!(observation.resources.schema_version, 1);
    assert_eq!(
        observation.route.observation_epoch,
        observation.resources.observation_epoch
    );
    assert_eq!(
        observation.route.configured_mode,
        StorageRolloutMode::Legacy
    );
    assert_eq!(
        observation.route.write_authority,
        StorageAuthority::LegacyV1
    );
    assert_eq!(observation.route.read_authority, StorageAuthority::LegacyV1);
    assert_eq!(observation.route.fallback_count, 0);
    assert_eq!(observation.route.mismatch_count, 0);
    assert_eq!(observation.route.shadow_comparison_count, 0);
    assert!(observation.resources.logical_cleanup_complete);
    Ok(())
}
