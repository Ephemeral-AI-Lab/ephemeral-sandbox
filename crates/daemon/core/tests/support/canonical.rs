use serde_json::{Map, Value};

const DROP_KEYS: &[&str] = &["timings", "daemon_pid", "uptime_s"];

pub fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out: Map<String, Value> = Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                if DROP_KEYS.contains(&key.as_str()) {
                    continue;
                }
                if let Some(v) = map.get(key) {
                    out.insert(key.clone(), canonicalize(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if n.as_i64().is_none() && n.as_u64().is_none() {
                    let q = (f * 1e9).round() / 1e9;
                    return serde_json::Number::from_f64(q).map_or(Value::Null, Value::Number);
                }
            }
            value.clone()
        }
        other => other.clone(),
    }
}

#[test]
fn drops_timings_pid_uptime_and_sorts_keys() {
    let a = serde_json::json!({
        "b": 2, "a": 1,
        "timings": {"x": 0.1},
        "daemon_pid": 1234,
        "uptime_s": 3.5,
        "nested": {"timings": {"y": 9.0}, "k": "v"}
    });
    let b = serde_json::json!({
        "a": 1, "b": 2,
        "timings": {"x": 999.9},
        "daemon_pid": 4321,
        "uptime_s": 88.0,
        "nested": {"k": "v", "timings": {"y": 0.0}}
    });
    assert_eq!(canonicalize(&a), canonicalize(&b));
}

#[test]
fn integers_preserved() {
    let v = serde_json::json!({"n": 0});
    assert_eq!(canonicalize(&v), serde_json::json!({"n": 0}));
}
