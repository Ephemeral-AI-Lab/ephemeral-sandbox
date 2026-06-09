//! JSON-column codec: serialize/deserialize TEXT-of-validated-JSON columns.
//!
//! There are two decode paths because the Rust stores disagree on NULL
//! handling (impl-eos-db.md §6/§8): [`decode_default`] mirrors `task_store`'s
//! `record.x or []` coercion (NULL/empty → `T::default()`); [`decode_opt`]
//! mirrors `agent_run_store`'s nullable columns that must preserve `None`.

use serde::{de::DeserializeOwned, Serialize};

use crate::error::DbError;

/// Serialize a value to its JSON TEXT column form.
///
/// # Errors
/// Returns [`DbError::JsonEncode`] if serialization fails.
pub(crate) fn encode<T: Serialize + ?Sized>(value: &T) -> Result<String, DbError> {
    serde_json::to_string(value).map_err(DbError::JsonEncode)
}

/// Default-to-empty decode for the `x or []` columns (iteration/attempt id
/// lists and execution-tree arrays). A
/// NULL/empty cell decodes to `T::default()`.
///
/// # Errors
/// Returns [`DbError::JsonDecode`] if a non-empty cell is malformed JSON.
pub(crate) fn decode_default<T: DeserializeOwned + Default>(
    text: Option<&str>,
) -> Result<T, DbError> {
    match text {
        None | Some("") => Ok(T::default()),
        Some(s) => serde_json::from_str(s).map_err(DbError::JsonDecode),
    }
}

/// Null-preserving decode for nullable JSON columns such as
/// `agent_runs.terminal_payload`: NULL/empty stays `None` — these columns do not
/// coerce to `[]`.
///
/// # Errors
/// Returns [`DbError::JsonDecode`] if a non-empty cell is malformed JSON.
pub(crate) fn decode_opt<T: DeserializeOwned>(text: Option<&str>) -> Result<Option<T>, DbError> {
    match text {
        None | Some("") => Ok(None),
        Some(s) => serde_json::from_str(s)
            .map(Some)
            .map_err(DbError::JsonDecode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eos_types::JsonObject;

    #[test]
    fn decode_default_coerces_null_and_empty() {
        assert_eq!(
            decode_default::<Vec<String>>(None).expect("none"),
            Vec::<String>::new()
        );
        assert_eq!(
            decode_default::<Vec<String>>(Some("")).expect("empty"),
            Vec::<String>::new()
        );
        assert_eq!(
            decode_default::<Vec<String>>(Some("[\"a\",\"b\"]")).expect("json"),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    #[test]
    fn decode_opt_preserves_none() {
        assert!(decode_opt::<JsonObject>(None).expect("none").is_none());
        assert!(decode_opt::<JsonObject>(Some("")).expect("empty").is_none());
        let obj = decode_opt::<JsonObject>(Some("{\"k\":1}"))
            .expect("json")
            .expect("some");
        assert_eq!(obj.get("k").and_then(serde_json::Value::as_i64), Some(1));
    }

    #[test]
    fn encode_roundtrips() {
        let v = vec!["x".to_owned()];
        let s = encode(&v).expect("encode");
        assert_eq!(s, "[\"x\"]");
        assert_eq!(decode_default::<Vec<String>>(Some(&s)).expect("decode"), v);
    }

    #[test]
    fn decode_default_rejects_malformed() {
        assert!(matches!(
            decode_default::<Vec<String>>(Some("not json")),
            Err(DbError::JsonDecode(_))
        ));
    }
}
