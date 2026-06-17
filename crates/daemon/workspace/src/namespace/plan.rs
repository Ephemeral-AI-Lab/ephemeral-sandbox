#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NamespacePlan {
    pub(crate) user: bool,
    pub(crate) mount: bool,
    pub(crate) pid: bool,
    pub(crate) network: NamespaceNetwork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamespaceNetwork {
    Host,
    Isolated,
}
