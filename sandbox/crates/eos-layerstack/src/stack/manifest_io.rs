use std::path::Path;

use eos_protocol::{LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};
use serde_json::{json, Value};

use crate::error::LayerStackError;
use crate::fsutil::check_layer_path;

pub(crate) fn read_manifest(path: impl AsRef<Path>) -> Result<Manifest, LayerStackError> {
    let path = path.as_ref();
    if !path.exists() {
        return Manifest::new(0, vec![], MANIFEST_SCHEMA_VERSION).map_err(LayerStackError::from);
    }
    let payload = std::fs::read_to_string(path)?;
    let value: Value =
        serde_json::from_str(&payload).map_err(|err| LayerStackError::Manifest(err.to_string()))?;
    let obj = value.as_object().ok_or_else(|| {
        LayerStackError::Manifest("manifest payload must be an object".to_owned())
    })?;
    let version = obj.get("version").and_then(Value::as_i64).ok_or_else(|| {
        LayerStackError::Manifest("manifest payload missing required field: version".to_owned())
    })?;
    let schema_version = obj
        .get("schema_version")
        .and_then(Value::as_i64)
        .unwrap_or(MANIFEST_SCHEMA_VERSION);
    if schema_version > MANIFEST_SCHEMA_VERSION {
        return Err(LayerStackError::Manifest(format!(
            "manifest schema_version is newer than this runtime supports: {schema_version}"
        )));
    }
    let raw_layers = obj.get("layers").and_then(Value::as_array).ok_or_else(|| {
        LayerStackError::Manifest("manifest payload missing required field: layers".to_owned())
    })?;
    let mut layers = Vec::with_capacity(raw_layers.len());
    for item in raw_layers {
        let item = item.as_object().ok_or_else(|| {
            LayerStackError::Manifest("manifest layer entries must be objects".to_owned())
        })?;
        let layer = LayerRef {
            layer_id: item
                .get("layer_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            path: item
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        validate_layer_ref(&layer)?;
        layers.push(layer);
    }
    Manifest::new(version, layers, schema_version).map_err(LayerStackError::from)
}

pub(crate) fn write_manifest(
    path: impl AsRef<Path>,
    manifest: &Manifest,
) -> Result<(), LayerStackError> {
    let value = json!({
        "schema_version": manifest.schema_version,
        "version": manifest.version,
        "layers": manifest
            .layers
            .iter()
            .map(|layer| json!({"layer_id": &layer.layer_id, "path": &layer.path}))
            .collect::<Vec<_>>(),
    });
    let encoded = serde_json::to_vec_pretty(&value)
        .map_err(|err| LayerStackError::Manifest(err.to_string()))?;
    super::write_atomic(path, &encoded)
}

pub(crate) fn validate_layer_ref(layer: &LayerRef) -> Result<(), LayerStackError> {
    if layer.layer_id.is_empty() {
        return Err(LayerStackError::Manifest(
            "layer_id must not be empty".to_owned(),
        ));
    }
    check_layer_path(&layer.path)
}
