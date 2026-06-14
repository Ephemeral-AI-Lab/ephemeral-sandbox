use std::path::{Path, PathBuf};

use layerstack::LayerPath;
use serde_json::{json, Value};

use crate::PluginRuntimeError;

pub(super) fn target_file_uri(
    projection_root: &Path,
    file_path: &str,
) -> Result<String, PluginRuntimeError> {
    target_file_uri_string(projection_root, file_path)
}

pub(super) fn target_file_uri_string(
    projection_root: &Path,
    file_path: &str,
) -> Result<String, PluginRuntimeError> {
    let normalized = LayerPath::parse(file_path).map_err(|err| {
        PluginRuntimeError::InvalidRequest(format!("invalid file_path for pyright_lsp: {err}"))
    })?;
    Ok(file_uri(&projection_root.join(normalized.as_str())))
}

pub(super) fn flatten_symbols(
    value: &Value,
    projection_root: &Path,
    fallback_file_path: &str,
    query: Option<&str>,
    out: &mut Vec<Value>,
) {
    let Some(items) = value.as_array() else {
        return;
    };
    for item in items {
        flatten_symbol_item(item, projection_root, fallback_file_path, query, out);
    }
}

fn flatten_symbol_item(
    item: &Value,
    projection_root: &Path,
    fallback_file_path: &str,
    query: Option<&str>,
    out: &mut Vec<Value>,
) {
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let matched = query.is_none_or(|query| name.to_ascii_lowercase().contains(query));
    let range = item
        .get("range")
        .cloned()
        .or_else(|| {
            item.get("location")
                .and_then(|location| location.get("range"))
                .cloned()
        })
        .unwrap_or(Value::Null);
    let selection_range = item
        .get("selectionRange")
        .cloned()
        .unwrap_or_else(|| range.clone());
    let file_path = item
        .get("location")
        .and_then(|location| location.get("uri"))
        .and_then(Value::as_str)
        .and_then(|uri| file_path_from_uri(uri, projection_root))
        .unwrap_or_else(|| fallback_file_path.to_owned());
    if matched && !name.is_empty() {
        out.push(json!({
            "name": name,
            "kind": item.get("kind").cloned().unwrap_or(Value::Null),
            "file_path": file_path,
            "range": range,
            "selection_range": selection_range,
        }));
    }
    if let Some(children) = item.get("children") {
        flatten_symbols(children, projection_root, fallback_file_path, query, out);
    }
}

pub(super) fn locations_from_lsp_result(result: &Value, projection_root: &Path) -> Vec<Value> {
    match result {
        Value::Null => Vec::new(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| location_from_lsp_value(item, projection_root))
            .collect(),
        Value::Object(_) => location_from_lsp_value(result, projection_root)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn location_from_lsp_value(value: &Value, projection_root: &Path) -> Option<Value> {
    if let Some(uri) = value.get("uri").and_then(Value::as_str) {
        return Some(json!({
            "uri": uri,
            "file_path": file_path_from_uri(uri, projection_root).unwrap_or_else(|| uri.to_owned()),
            "range": value.get("range").cloned().unwrap_or(Value::Null),
        }));
    }
    if let Some(uri) = value.get("targetUri").and_then(Value::as_str) {
        return Some(json!({
            "uri": uri,
            "file_path": file_path_from_uri(uri, projection_root).unwrap_or_else(|| uri.to_owned()),
            "range": value.get("targetRange").cloned().unwrap_or(Value::Null),
            "selection_range": value.get("targetSelectionRange").cloned().unwrap_or(Value::Null),
        }));
    }
    None
}

pub(super) fn diagnostic_value(uri: &str, projection_root: &Path, diagnostic: Value) -> Value {
    json!({
        "uri": uri,
        "file_path": file_path_from_uri(uri, projection_root).unwrap_or_else(|| uri.to_owned()),
        "range": diagnostic.get("range").cloned().unwrap_or(Value::Null),
        "severity": diagnostic.get("severity").cloned().unwrap_or(Value::Null),
        "code": diagnostic.get("code").cloned().unwrap_or(Value::Null),
        "source": diagnostic.get("source").cloned().unwrap_or(Value::Null),
        "message": diagnostic.get("message").cloned().unwrap_or(Value::Null),
        "raw": diagnostic,
    })
}

pub(super) fn file_uri(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut uri = String::from("file://");
    for byte in raw.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(char::from(*byte))
            }
            other => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                uri.push('%');
                uri.push(char::from(HEX[usize::from(other >> 4)]));
                uri.push(char::from(HEX[usize::from(other & 0x0f)]));
            }
        }
    }
    uri
}

fn file_path_from_uri(uri: &str, projection_root: &Path) -> Option<String> {
    let path = uri.strip_prefix("file://")?;
    let decoded = percent_decode(path)?;
    let path = PathBuf::from(decoded);
    path.strip_prefix(projection_root)
        .ok()
        .map(|path| path.to_string_lossy().trim_start_matches('/').to_owned())
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = *bytes.get(index + 1)?;
            let lo = *bytes.get(index + 2)?;
            out.push((hex_value(hi)? << 4) | hex_value(lo)?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
