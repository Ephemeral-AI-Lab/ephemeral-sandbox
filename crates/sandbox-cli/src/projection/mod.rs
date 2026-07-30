pub mod document;

#[cfg(feature = "manager")]
pub mod manager;
#[cfg(feature = "observability")]
pub mod observability;
#[cfg(feature = "runtime")]
pub mod runtime;

use sandbox_operation_contract::OperationDomain;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgumentProjection {
    pub name: &'static str,
    pub flag: Option<&'static str>,
    pub additional_flags: &'static [&'static str],
    pub value_file_flag: Option<&'static str>,
    pub positional: Option<&'static str>,
}

impl ArgumentProjection {
    #[must_use]
    pub const fn flag(name: &'static str, flag: &'static str) -> Self {
        Self {
            name,
            flag: Some(flag),
            additional_flags: &[],
            value_file_flag: None,
            positional: None,
        }
    }

    #[must_use]
    pub const fn flag_with_value_file(
        name: &'static str,
        flag: &'static str,
        value_file_flag: &'static str,
    ) -> Self {
        Self {
            name,
            flag: Some(flag),
            additional_flags: &[],
            value_file_flag: Some(value_file_flag),
            positional: None,
        }
    }

    #[must_use]
    pub const fn flag_with_additional(
        name: &'static str,
        flag: &'static str,
        additional_flags: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            flag: Some(flag),
            additional_flags,
            value_file_flag: None,
            positional: None,
        }
    }

    #[must_use]
    pub const fn positional(name: &'static str, positional: &'static str) -> Self {
        Self {
            name,
            flag: None,
            additional_flags: &[],
            value_file_flag: None,
            positional: Some(positional),
        }
    }

    #[must_use]
    pub fn accepts_flag(&self, flag: &str) -> bool {
        self.flag == Some(flag)
            || self.additional_flags.contains(&flag)
            || self.value_file_flag == Some(flag)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationProjection {
    pub name: &'static str,
    pub path: &'static [&'static str],
    pub usage: &'static str,
    pub examples: &'static [&'static str],
    pub arguments: &'static [ArgumentProjection],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogProjection {
    pub operation_execution_space: OperationDomain,
    pub operations: &'static [OperationProjection],
}
