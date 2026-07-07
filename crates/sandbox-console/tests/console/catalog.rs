use http::StatusCode;
use serde_json::Value;

use crate::support;

fn operation_names(catalog: &Value) -> Vec<String> {
    catalog["operations"]
        .as_array()
        .expect("operations array")
        .iter()
        .map(|spec| spec["name"].as_str().expect("operation name").to_owned())
        .collect()
}

#[tokio::test]
async fn catalog_returns_all_three_execution_spaces() {
    let gateway = support::FakeGateway::spawn(|_| Vec::new()).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::get(console, "/api/catalog").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = support::body_json(response).await;

    let manager_ops = operation_names(&body["manager"]);
    assert!(manager_ops.contains(&"create_sandbox".to_owned()));
    assert!(manager_ops.contains(&"list_sandboxes".to_owned()));
    assert!(manager_ops.contains(&"checkpoint_squash".to_owned()));

    let runtime_ops = operation_names(&body["runtime"]);
    assert!(runtime_ops.contains(&"exec_command".to_owned()));
    assert!(runtime_ops.contains(&"read_command_lines".to_owned()));
    assert!(runtime_ops.contains(&"file_read".to_owned()));
    assert!(!runtime_ops.contains(&"file_list".to_owned()));

    let observability_ops = operation_names(&body["observability"]);
    assert!(observability_ops.contains(&"snapshot".to_owned()));
    assert!(observability_ops.contains(&"trace".to_owned()));
    assert!(observability_ops.contains(&"layerstack".to_owned()));

    assert_eq!(gateway.request_count(), 0, "catalog never hits the gateway");
}
