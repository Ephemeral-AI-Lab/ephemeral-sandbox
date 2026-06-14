mod command;
mod lsp_values;
mod ops;
mod process;
mod projection;
mod responses;
mod runtime;

use config::configs::daemon::PYRIGHT_LSP_PLUGIN_ID;

pub(super) use self::runtime::PyrightLspRuntime;

const FRESHNESS_ANALYZER_REFLECTED: &str = "analyzer_reflected";
const LANGUAGE_ID: &str = "python";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinPluginProvider {
    PyrightLsp,
}

impl BuiltinPluginProvider {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::PyrightLsp => PYRIGHT_LSP_PLUGIN_ID,
        }
    }
}
