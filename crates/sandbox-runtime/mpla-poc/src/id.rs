use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{PocError, PocResult};

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4().to_string())
            }

            pub fn from_string(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

opaque_id!(ActivationOperationId);
opaque_id!(AllocationId);
opaque_id!(OperationId);
opaque_id!(PublicationId);
opaque_id!(SessionId);

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    pub fn parse(value: impl Into<String>) -> PocResult<Self> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = (1..=64).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !valid {
            return Err(PocError::InvalidRunId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
