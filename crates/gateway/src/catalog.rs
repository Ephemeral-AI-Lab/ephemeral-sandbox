use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use protocol::catalog::{HostVerb, OpVisibility, ServedBy, BUILTIN_OPS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Visibility {
    Public,
    Operator,
    Internal,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Host(HostVerb),
    Daemon,
}

#[derive(Debug)]
pub(crate) struct OpEntry {
    pub(crate) name: String,
    pub(crate) family: &'static str,
    pub(crate) route: Route,
    pub(crate) visibility: Visibility,
    pub(crate) mutates_state: bool,
}

pub(crate) struct Catalog {
    by_name: HashMap<String, Arc<OpEntry>>,
}

impl Catalog {
    pub(crate) fn load_builtin() -> Result<Self> {
        let mut by_name = HashMap::new();
        for contract in BUILTIN_OPS {
            let route = match contract.served_by {
                ServedBy::Daemon => Route::Daemon,
                ServedBy::Host => Route::Host(contract.host_verb.ok_or_else(|| {
                    anyhow::anyhow!("host-served op {} has no host verb", contract.name)
                })?),
            };
            let visibility = match contract.visibility {
                OpVisibility::Public => Visibility::Public,
                OpVisibility::Operator => Visibility::Operator,
                OpVisibility::Internal => Visibility::Internal,
                OpVisibility::Test => Visibility::Test,
            };
            let entry = Arc::new(OpEntry {
                name: contract.name.to_owned(),
                family: contract.family.as_str(),
                route,
                visibility,
                mutates_state: contract.mutates_state,
            });
            if by_name.insert(contract.name.to_owned(), entry).is_some() {
                bail!("catalog name claimed twice: {}", contract.name);
            }
        }
        Ok(Self { by_name })
    }

    pub(crate) fn lookup(&self, op: &str) -> Option<&Arc<OpEntry>> {
        self.by_name.get(op)
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<&Arc<OpEntry>> {
        self.by_name.values().collect()
    }
}

impl Visibility {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Operator => "operator",
            Self::Internal => "internal",
            Self::Test => "test",
        }
    }
}
