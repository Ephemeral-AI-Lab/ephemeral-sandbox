use crate::{Error, ErrorKind, FieldClass, ROOT_FORMAT_V2};

pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_COMPONENT_BYTES: usize = 255;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CanonicalPath(Vec<u8>);

impl CanonicalPath {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Error> {
        Self::validate(&bytes)?;
        Ok(Self(bytes))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        Self::validate(bytes)?;
        Ok(Self(bytes.to_vec()))
    }

    fn validate(bytes: &[u8]) -> Result<(), Error> {
        if bytes.is_empty() || bytes.len() > MAX_PATH_BYTES {
            return Err(Error::new(
                if bytes.len() > MAX_PATH_BYTES {
                    ErrorKind::LimitExceeded
                } else {
                    ErrorKind::InvalidValue
                },
                ROOT_FORMAT_V2,
                FieldClass::Path,
                u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            ));
        }
        if bytes.first() == Some(&b'/') || bytes.last() == Some(&b'/') {
            return Err(Error::new(
                ErrorKind::InvalidValue,
                ROOT_FORMAT_V2,
                FieldClass::Path,
                0,
            ));
        }
        for (index, component) in bytes.split(|byte| *byte == b'/').enumerate() {
            if component.is_empty()
                || component == b"."
                || component == b".."
                || component.len() > MAX_COMPONENT_BYTES
                || component.contains(&0)
            {
                return Err(Error::new(
                    if component.len() > MAX_COMPONENT_BYTES {
                        ErrorKind::LimitExceeded
                    } else {
                        ErrorKind::InvalidValue
                    },
                    ROOT_FORMAT_V2,
                    FieldClass::Path,
                    u32::try_from(index).unwrap_or(u32::MAX),
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}
