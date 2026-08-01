#![allow(dead_code)]

#[path = "../src/engine.rs"]
mod engine;
#[path = "../src/labels.rs"]
mod labels;

use engine::DockerEngine;
use sandbox_config::configs::manager::DockerRuntimeConfig;

#[test]
fn resource_metrics_batches_reuse_one_lazy_executor() {
    let engine = DockerEngine::new(DockerRuntimeConfig::default());
    assert!(engine
        .resource_metrics_executor
        .lock()
        .expect("resource metrics executor lock")
        .is_none());

    assert!(engine
        .container_resource_metrics_batch(Vec::new())
        .expect("first empty metrics batch")
        .is_empty());
    let first_executor = engine
        .resource_metrics_executor
        .lock()
        .expect("resource metrics executor lock")
        .as_ref()
        .map(|executor| std::ptr::from_ref(executor).addr())
        .expect("resource metrics executor");

    assert!(engine
        .container_resource_metrics_batch(Vec::new())
        .expect("second empty metrics batch")
        .is_empty());
    let second_executor = engine
        .resource_metrics_executor
        .lock()
        .expect("resource metrics executor lock")
        .as_ref()
        .map(|executor| std::ptr::from_ref(executor).addr())
        .expect("resource metrics executor");

    assert_eq!(first_executor, second_executor);
}

#[test]
fn resource_metrics_batch_uses_safe_fallback_inside_tokio_runtime() {
    let engine = DockerEngine::new(DockerRuntimeConfig::default());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    runtime.block_on(async {
        assert!(engine
            .container_resource_metrics_batch(Vec::new())
            .expect("empty metrics batch inside Tokio runtime")
            .is_empty());
    });
    assert!(engine
        .resource_metrics_executor
        .lock()
        .expect("resource metrics executor lock")
        .is_none());
}

#[test]
fn initialized_resource_metrics_executor_drops_inside_tokio_runtime() {
    let engine = DockerEngine::new(DockerRuntimeConfig::default());
    assert!(engine
        .container_resource_metrics_batch(Vec::new())
        .expect("initialize resource metrics executor")
        .is_empty());
    assert!(engine
        .resource_metrics_executor
        .lock()
        .expect("resource metrics executor lock")
        .is_some());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("outer test runtime");
    runtime.block_on(async move {
        drop(engine);
    });
}
