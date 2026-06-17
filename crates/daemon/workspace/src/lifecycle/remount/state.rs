#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkspaceRemountState {
    #[default]
    Active,
    Pending,
}

impl WorkspaceRemountState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pending => "remount_pending",
        }
    }
}
