//! Golden CAS byte-identity fixtures (AV-1c). ALL 18 cases must pass; the
//! unicode cases prove the `ensure_ascii=True` escaper. Fixtures are immutable
//! ground truth produced by the live Rust — never edit them to match code.

use base64::Engine as _;
use layerstack::{layer_digest, manifest_root_hash, LayerChange, LayerPath, LayerRef, Manifest};
use proptest as _;
use serde as _;
use serde_json::Value;
use sha2 as _;
use thiserror as _;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const CASES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/cas/cases.json"
));

fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message.into()).into()
}

fn field<'a>(value: &'a Value, name: &str) -> TestResult<&'a Value> {
    value
        .get(name)
        .ok_or_else(|| test_error(format!("missing fixture field `{name}`")))
}

fn str_field<'a>(value: &'a Value, name: &str) -> TestResult<&'a str> {
    field(value, name)?
        .as_str()
        .ok_or_else(|| test_error(format!("fixture field `{name}` must be a string")))
}

fn array_field<'a>(value: &'a Value, name: &str) -> TestResult<&'a Vec<Value>> {
    field(value, name)?
        .as_array()
        .ok_or_else(|| test_error(format!("fixture field `{name}` must be an array")))
}

fn build_layer_change(c: &Value) -> TestResult<LayerChange> {
    let kind = str_field(c, "kind")?;
    let path = LayerPath::parse(str_field(c, "path")?)?;
    match kind {
        "write" => {
            let content = base64::engine::general_purpose::STANDARD
                .decode(str_field(c, "write_content_b64")?)?;
            Ok(LayerChange::Write { path, content })
        }
        "delete" => Ok(LayerChange::Delete { path }),
        "symlink" => Ok(LayerChange::Symlink {
            path,
            source_path: str_field(c, "source_path")?.to_owned(),
        }),
        "opaque_dir" => Ok(LayerChange::OpaqueDir { path }),
        other => Err(test_error(format!("unknown change kind: {other}"))),
    }
}

#[test]
fn all_cas_fixtures_match() -> TestResult {
    let cases: Vec<Value> = serde_json::from_str(CASES)?;
    assert_eq!(cases.len(), 18, "expected 18 golden cases");

    let mut checked = 0usize;
    for case in &cases {
        let name = str_field(case, "name")?;
        let kind = str_field(case, "kind")?;
        let expected = str_field(case, "expected")?;

        let actual = match kind {
            "manifest_root_hash" => {
                let layers: Vec<LayerRef> = array_field(field(case, "input")?, "layers")?
                    .iter()
                    .map(|layer| {
                        Ok(LayerRef {
                            layer_id: str_field(layer, "layer_id")?.to_owned(),
                            path: str_field(layer, "path")?.to_owned(),
                        })
                    })
                    .collect::<TestResult<_>>()?;
                let version = i64::try_from(layers.len())?;
                let manifest = Manifest::new(version, layers, 1)?;
                manifest_root_hash(&manifest)
            }
            "layer_digest" => {
                let changes: Vec<LayerChange> = array_field(field(case, "input")?, "changes")?
                    .iter()
                    .map(build_layer_change)
                    .collect::<TestResult<_>>()?;
                // Cross-check the documented aggregate ordering too.
                if let Some(order) = case["aggregated_order"].as_array() {
                    let agg = layerstack::aggregate_layer_changes(&changes);
                    let agg_paths: Vec<&str> = agg.iter().map(|c| c.path().as_str()).collect();
                    let expected_order: Vec<&str> = order
                        .iter()
                        .map(|value| {
                            value.as_str().ok_or_else(|| {
                                test_error("aggregated_order entries must be strings")
                            })
                        })
                        .collect::<TestResult<_>>()?;
                    assert_eq!(agg_paths, expected_order, "aggregate order for {name}");
                }
                layer_digest(&changes)
            }
            other => return Err(test_error(format!("unknown case kind: {other}"))),
        };

        assert_eq!(actual, expected, "hash mismatch for case {name}");
        checked += 1;
    }
    assert_eq!(checked, 18);
    Ok(())
}
