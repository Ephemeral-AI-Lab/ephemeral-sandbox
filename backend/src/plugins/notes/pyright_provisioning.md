# Pyright LSP Provisioning

**Current contract.** The LSP plugin uses `pyright-langserver --stdio` inside
the sandbox. There is no alternate language-server fallback.

## Setup Path

`sandbox.ephemeral_workspace.plugin.install` uploads the LSP plugin bundle plus
host-prepared setup packages. Node and Pyright artifacts are downloaded on the
host, then sent into the sandbox with provider `put_archive` under
`/eos/plugin-packages/lsp`.

`setup.sh` runs offline. It extracts the uploaded Node archive into
`/eos/plugin-packages/lsp/node` only when `node`/`npm` are not already
available. After Node is available, setup installs the uploaded Pyright tarball
with npm:

```bash
npm install -g --offline --cache /eos/plugin-packages/lsp/npm-cache --omit=optional /eos/plugin-packages/lsp/pyright.tgz
```

The marker is `.pyright_installed`, but setup also verifies
`pyright-langserver` is on `PATH` before short-circuiting.

## Runtime Path

`runtime/pyright_session.py` owns the subprocess command:

```bash
pyright-langserver --stdio
```

If the sandbox has the standard `testbed` conda environment, the session starts
through that environment and prepends `/eos/plugin-packages/lsp/node/bin` to
`PATH`. Missing setup fails closed rather than falling back to a different
language server.

## Diagnostics

Diagnostics use Pyright's synchronous `textDocument/diagnostic` request. Normal
calls return the current diagnostic report directly. `wait_for_diagnostics=true`
is reserved for scenarios that intentionally expect a diagnostic and will poll
for a non-empty report up to the session timeout.
