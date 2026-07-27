use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{PocError, PocResult};

pub fn replace_json<T: Serialize>(_path: &Path, _value: &T) -> PocResult<()> {
    Err(PocError::Unsupported(
        "durable selector implementation is assigned to M0 Worker B".to_owned(),
    ))
}

pub fn read_json<T: DeserializeOwned>(_path: &Path) -> PocResult<T> {
    Err(PocError::Unsupported(
        "durable selector implementation is assigned to M0 Worker B".to_owned(),
    ))
}
