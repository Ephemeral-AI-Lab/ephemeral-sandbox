#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationFamily {
    Command,
    File,
    Workspace,
    Health,
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    String,
    Integer,
    Float,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgCliSpec {
    pub flag: Option<&'static str>,
    pub positional: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgSpec {
    pub name: &'static str,
    pub kind: ArgKind,
    pub required: bool,
    pub help: &'static str,
    pub default: Option<&'static str>,
    pub cli: Option<ArgCliSpec>,
}

impl ArgSpec {
    #[must_use]
    pub const fn required(
        name: &'static str,
        kind: ArgKind,
        help: &'static str,
        cli: Option<ArgCliSpec>,
    ) -> Self {
        Self {
            name,
            kind,
            required: true,
            help,
            default: None,
            cli,
        }
    }

    #[must_use]
    pub const fn optional(
        name: &'static str,
        kind: ArgKind,
        help: &'static str,
        default: Option<&'static str>,
        cli: Option<ArgCliSpec>,
    ) -> Self {
        Self {
            name,
            kind,
            required: false,
            help,
            default,
            cli,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliSpec {
    pub path: &'static [&'static str],
    pub usage: &'static str,
    pub examples: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationSpec {
    pub name: &'static str,
    pub family: OperationFamily,
    pub summary: &'static str,
    pub args: &'static [ArgSpec],
    pub cli: Option<CliSpec>,
}
