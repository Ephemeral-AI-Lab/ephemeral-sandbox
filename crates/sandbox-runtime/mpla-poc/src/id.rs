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

macro_rules! digest_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> PocResult<Self> {
                let value = value.into();
                let valid = value.len() == 64
                    && value
                        .as_bytes()
                        .iter()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
                if !valid {
                    return Err(PocError::Integrity(format!(
                        "{} must be 64 lowercase hexadecimal characters",
                        stringify!($name)
                    )));
                }
                Ok(Self(value))
            }

            pub fn from_digest_bytes(bytes: [u8; 32]) -> Self {
                let mut value = String::with_capacity(64);
                for byte in bytes {
                    use fmt::Write as _;
                    write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
                }
                Self(value)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

digest_id!(AttributionRootId);
digest_id!(RootId);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct LocatorGeneration(u64);

impl LocatorGeneration {
    pub const INITIAL: Self = Self(1);

    pub fn new(value: u64) -> PocResult<Self> {
        if value == 0 {
            return Err(PocError::Integrity(
                "locator generation must be non-zero".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_next(self) -> PocResult<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| PocError::Integrity("locator generation overflow".to_owned()))
    }
}

impl fmt::Display for LocatorGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct RefSequence(u64);

impl RefSequence {
    pub const ZERO: Self = Self(0);

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_next(self) -> PocResult<Self> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or_else(|| PocError::Integrity("ref sequence overflow".to_owned()))
    }
}

impl fmt::Display for RefSequence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

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
