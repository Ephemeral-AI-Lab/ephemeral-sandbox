//! Static first-party plugin provider runtime.
//!
//! The public plugin family is intentionally fixed: cataloged
//! `sandbox.plugin.*` operations route to daemon-owned providers such as
//! `pyright_lsp`. There is no manifest upload, runtime operation registration,
//! or dynamic `plugin.*` dispatch fallback in this crate.
#![forbid(unsafe_code)]

mod pyright_lsp;
mod state;

pub use self::pyright_lsp::BuiltinPluginProvider;
pub use self::state::PluginRuntime;

/// Failures surfaced by static plugin providers. The daemon folds each variant
/// onto its own error algebra or typed rejected envelope.
#[derive(Debug, thiserror::Error)]
pub enum PluginRuntimeError {
    /// The caller currently owns an isolated workspace handle and may not use
    /// shared plugin provider ops.
    #[error("plugin ops are forbidden while caller has an isolated workspace")]
    ForbiddenInIsolatedWorkspace,

    /// The runtime's state mutex was poisoned.
    #[error("daemon state lock poisoned: {0}")]
    StateLockPoisoned(&'static str),

    /// A structurally invalid request reached the runtime.
    #[error("{0}")]
    InvalidRequest(String),

    /// A filesystem / socket I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// The layer-stack storage / lease layer failed.
    #[error(transparent)]
    LayerStack(#[from] layerstack::LayerStackError),

    /// A first-party plugin provider is disabled by daemon config.
    #[error("plugin provider {0} is disabled")]
    PluginDisabled(String),

    /// The static Pyright LSP provider failed.
    #[error("pyright_lsp failed: {0}")]
    PyrightLsp(String),
}
