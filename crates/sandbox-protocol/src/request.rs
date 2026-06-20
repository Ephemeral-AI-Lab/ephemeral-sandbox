use serde_json::{json, Map, Value};

use crate::error_kind;
use crate::response::Response;

#[derive(Debug, Clone, PartialEq)]
pub struct OwnedRequest {
    pub op: String,
    pub request_id: String,
    pub args: Value,
}

pub type RpcRequest = OwnedRequest;

impl OwnedRequest {
    #[must_use]
    pub fn as_request(&self) -> Request<'_> {
        Request::new(&self.op, &self.request_id, &self.args)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub name: &'a str,
    pub request_id: &'a str,
    pub args: &'a Value,
}

impl<'a> Request<'a> {
    #[must_use]
    pub const fn new(name: &'a str, request_id: &'a str, args: &'a Value) -> Self {
        Self {
            name,
            request_id,
            args,
        }
    }

    pub fn required_string(&self, field: &str) -> Result<String, Response> {
        self.field(field).and_then(|value| match value.as_str() {
            Some(value) if !value.is_empty() => Ok(value.to_owned()),
            Some(_) => Err(self.invalid_argument(format!("{field} must be non-empty"))),
            None => Err(self.invalid_argument(format!("{field} must be a string"))),
        })
    }

    pub fn optional_string(&self, field: &str) -> Result<Option<String>, Response> {
        match self.optional_field(field)? {
            Some(value) => match value.as_str() {
                Some(value) => Ok(Some(value.to_owned())),
                None => Err(self.invalid_argument(format!("{field} must be a string"))),
            },
            None => Ok(None),
        }
    }

    pub fn required_path(&self, field: &str) -> Result<std::path::PathBuf, Response> {
        Ok(std::path::PathBuf::from(self.required_string(field)?))
    }

    pub fn optional_path(&self, field: &str) -> Result<Option<std::path::PathBuf>, Response> {
        Ok(self.optional_string(field)?.map(std::path::PathBuf::from))
    }

    pub fn optional_u64(&self, field: &str) -> Result<Option<u64>, Response> {
        match self.optional_field(field)? {
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or_else(|| self.invalid_argument(format!("{field} must be an integer"))),
            None => Ok(None),
        }
    }

    pub fn required_u64(&self, field: &str) -> Result<u64, Response> {
        self.field(field).and_then(|value| {
            value
                .as_u64()
                .ok_or_else(|| self.invalid_argument(format!("{field} must be an integer")))
        })
    }

    pub fn optional_usize(&self, field: &str) -> Result<Option<usize>, Response> {
        self.optional_u64(field)?
            .map(|value| {
                usize::try_from(value)
                    .map_err(|_| self.invalid_argument(format!("{field} is too large")))
            })
            .transpose()
    }

    pub fn required_usize(&self, field: &str) -> Result<usize, Response> {
        usize::try_from(self.required_u64(field)?)
            .map_err(|_| self.invalid_argument(format!("{field} is too large")))
    }

    pub fn optional_f64(&self, field: &str) -> Result<Option<f64>, Response> {
        match self.optional_field(field)? {
            Some(value) => match value.as_f64() {
                Some(value) if value.is_finite() => Ok(Some(value)),
                Some(_) => Err(self.invalid_argument(format!("{field} must be finite"))),
                None => Err(self.invalid_argument(format!("{field} must be a number"))),
            },
            None => Ok(None),
        }
    }

    fn field(&self, field: &str) -> Result<&Value, Response> {
        self.args_object()?
            .get(field)
            .ok_or_else(|| self.invalid_argument(format!("{field} is required for {}", self.name)))
    }

    fn optional_field(&self, field: &str) -> Result<Option<&Value>, Response> {
        Ok(self.args_object()?.get(field))
    }

    fn args_object(&self) -> Result<&Map<String, Value>, Response> {
        self.args
            .as_object()
            .ok_or_else(|| self.invalid_argument("args must be an object"))
    }

    pub fn invalid_argument(&self, message: impl Into<String>) -> Response {
        Response::fault(error_kind::INVALID_REQUEST, message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgsPresence {
    Required,
    OptionalEmptyObject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestDecodeError {
    kind: &'static str,
    message: String,
}

impl RequestDecodeError {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

pub fn decode_request_object(
    mut object: Map<String, Value>,
    args_presence: ArgsPresence,
) -> Result<OwnedRequest, RequestDecodeError> {
    let op = remove_request_string(&mut object, "op")?;
    let request_id = remove_request_string(&mut object, "request_id")?;
    let args = match object.remove("args") {
        Some(args) => args,
        None if args_presence == ArgsPresence::OptionalEmptyObject => json!({}),
        None => {
            return Err(invalid_request(
                "request must include op, request_id, and args",
            ));
        }
    };
    if op.trim().is_empty() {
        return Err(invalid_request("op is required"));
    }
    if !args.is_object() {
        return Err(invalid_request("args must be an object"));
    }
    Ok(OwnedRequest {
        op,
        request_id,
        args,
    })
}

fn remove_request_string(
    object: &mut Map<String, Value>,
    field: &str,
) -> Result<String, RequestDecodeError> {
    let Some(Value::String(value)) = object.remove(field) else {
        return Err(invalid_request(format!(
            "{field} is required and must be a string"
        )));
    };
    Ok(value)
}

fn invalid_request(message: impl Into<String>) -> RequestDecodeError {
    RequestDecodeError {
        kind: error_kind::INVALID_REQUEST,
        message: message.into(),
    }
}
