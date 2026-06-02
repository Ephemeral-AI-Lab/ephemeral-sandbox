#!/usr/bin/env bash
# Idempotent Node 22 + Pyright install for the LSP plugin.

set -eu

PLUGIN_DIR="${EOS_PLUGIN_DIR:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}"
PACKAGE_DIR="${EOS_PLUGIN_PACKAGE_DIR:-/eos/plugin-packages/lsp}"
NODE_HOME="${EOS_NODE_HOME:-$PACKAGE_DIR/node}"
PYRIGHT_VERSION="${EOS_PYRIGHT_VERSION:-1.1.409}"
MARKER="$PLUGIN_DIR/.pyright_installed"

export PATH="$NODE_HOME/bin:$PATH"

if [ -f "$MARKER" ] && command -v pyright-langserver >/dev/null 2>&1; then
    exit 0
fi

if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
    if [ ! -s "$PACKAGE_DIR/node.tar.xz" ]; then
        echo "missing host-uploaded Node archive: $PACKAGE_DIR/node.tar.xz" >&2
        exit 35
    fi
    mkdir -p "$NODE_HOME"
    tar -xJf "$PACKAGE_DIR/node.tar.xz" -C "$NODE_HOME" --strip-components=1
fi

export PATH="$NODE_HOME/bin:$PATH"
npm config set prefix "$NODE_HOME"
if ! command -v pyright-langserver >/dev/null 2>&1; then
    if [ ! -s "$PACKAGE_DIR/pyright.tgz" ]; then
        echo "missing host-uploaded Pyright package: $PACKAGE_DIR/pyright.tgz" >&2
        exit 36
    fi
    npm install -g --offline --cache "$PACKAGE_DIR/npm-cache" --omit=optional "$PACKAGE_DIR/pyright.tgz"
fi

node -v
npm -v
pyright --version
command -v pyright-langserver >/dev/null

mkdir -p "$(dirname "$MARKER")"
: > "$MARKER"
