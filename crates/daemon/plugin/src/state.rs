use std::sync::{Arc, Mutex, MutexGuard};

use config::configs::daemon::PluginRuntimeConfig;
use layerstack::CommitOptions;
use workspace::NsRunnerLauncher;

use crate::pyright_lsp::PyrightLspRuntime;
use crate::PluginRuntimeError;

/// Instance-owned static plugin provider runtime.
pub struct PluginRuntime {
    pub(super) config: PluginRuntimeConfig,
    pyright_lsp: Mutex<PyrightLspRuntime>,
}

impl PluginRuntime {
    /// Build a plugin runtime over its typed config.
    #[must_use]
    pub fn new(config: PluginRuntimeConfig, launcher: Arc<dyn NsRunnerLauncher>) -> Self {
        Self::with_commit_options(config, launcher, CommitOptions::default())
    }

    #[must_use]
    pub fn with_commit_options(
        config: PluginRuntimeConfig,
        _launcher: Arc<dyn NsRunnerLauncher>,
        _commit_options: CommitOptions,
    ) -> Self {
        Self {
            pyright_lsp: Mutex::new(PyrightLspRuntime::new(&config.pyright_lsp)),
            config,
        }
    }

    pub(super) fn lock_pyright_lsp(
        &self,
    ) -> Result<MutexGuard<'_, PyrightLspRuntime>, PluginRuntimeError> {
        self.pyright_lsp
            .lock()
            .map_err(|_| PluginRuntimeError::StateLockPoisoned("pyright_lsp runtime"))
    }
}
