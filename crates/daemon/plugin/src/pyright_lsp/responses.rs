use config::configs::daemon::PYRIGHT_LSP_PLUGIN_ID;
use serde_json::{json, Value};

use super::FRESHNESS_ANALYZER_REFLECTED;

pub(super) fn base_pyright_response(manifest_key: &str, fields: Value) -> Value {
    let mut response = json!({
        "success": true,
        "provider": PYRIGHT_LSP_PLUGIN_ID,
        "manifest_key": manifest_key,
        "freshness": FRESHNESS_ANALYZER_REFLECTED,
        "stale": false,
        "analysis_status": "reflected",
    });
    if let (Some(target), Some(source)) = (response.as_object_mut(), fields.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    response
}

pub(super) fn pyright_timeout_response(manifest_key: &str, message: String) -> Value {
    json!({
        "success": true,
        "provider": PYRIGHT_LSP_PLUGIN_ID,
        "manifest_key": manifest_key,
        "freshness": FRESHNESS_ANALYZER_REFLECTED,
        "stale": true,
        "analysis_status": "timeout",
        "diagnostics": [],
        "error": {
            "kind": "analysis_timeout",
            "message": message,
        },
    })
}
