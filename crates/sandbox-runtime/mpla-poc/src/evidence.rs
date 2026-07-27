use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{PocError, PocResult};

pub fn write_atomic_json<T: Serialize>(_path: &Path, _value: &T) -> PocResult<()> {
    Err(PocError::Unsupported(
        "atomic evidence implementation is assigned to M0 Worker A".to_owned(),
    ))
}

pub fn read_json<T: DeserializeOwned>(_path: &Path) -> PocResult<T> {
    Err(PocError::Unsupported(
        "evidence reader implementation is assigned to M0 Worker A".to_owned(),
    ))
}
