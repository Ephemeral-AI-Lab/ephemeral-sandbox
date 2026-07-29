<div align="center">


# Ephemeral Sandbox: Coding-Agent Swarm Infrastructure

**Raise the concurrency ceiling for coding-agent swarms with isolated workspace
sessions inside one shared sandbox.**

[Docs](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-docs) ·
[Quick start](#quick-start) ·
[MCP](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-docs/tree/main/mcp) ·
[CLI](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-docs/tree/main/cli) ·
[Architecture](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-docs/blob/main/architecture/00-foundations/01-system-overview.md) ·
[Tests](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-test)

<img src="assets/mascot.png" alt="Ephemeral Sandbox mascot: a Siamese cat in a sandbox" width="280">

[![License: Apache 2.0](https://img.shields.io/badge/license-Apache%202.0-111111.svg)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.com/invite/WdDJ3tru9)


</div>

Ephemeral Sandbox is open-source coding-agent swarm infrastructure: many
parallel agents collaborate, each through an isolated workspace session inside
one shared sandbox, replacing single-checkout concurrency limits with safe,
reviewable publishing.

This repository contains the headless Rust core: gateway, manager, daemon,
runtime, observability, CLI, and MCP components. The browser UI and its backend
live in the separate
[Ephemeral Sandbox Console](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-console)
repository.

## Why Ephemeral Sandbox?

- **Work in parallel.** Run multiple coding agents at the same time.
- **Stay isolated.** Give every agent a private writable workspace session over
  one stable project base.
- **Publish with confidence.** Inspect activity and change provenance before
  publishing a complete resolved change set.

## Quick start

Choose your host OS. Docker must already be installed and reachable. For full
commands, CLI verification, and troubleshooting, use the detailed setup guides:
[Linux](docs/linux-setup.md), [macOS](docs/macos-setup.md), or
[Windows](docs/windows-setup.md).

### Linux amd64

```sh
curl -LO https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox/releases/latest/download/ephemeral-sandbox-linux-amd64.tar.gz
tar -xzf ephemeral-sandbox-linux-amd64.tar.gz
cd ephemeral-sandbox-linux-amd64
nohup bin/start-sandbox-linux-docker-gateway >/tmp/eos-gateway.log 2>&1 &
```

More details: [Linux setup](docs/linux-setup.md).

### macOS arm64 (Apple silicon)

```sh
curl -LO https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox/releases/latest/download/ephemeral-sandbox-macos-arm64.tar.gz
tar -xzf ephemeral-sandbox-macos-arm64.tar.gz
cd ephemeral-sandbox-macos-arm64
bin/start-sandbox-macos-docker-gateway
```

More details: [macOS setup](docs/macos-setup.md).

### Windows amd64

```powershell
curl.exe -LO https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox/releases/latest/download/ephemeral-sandbox-windows-amd64.zip
tar.exe -xf ephemeral-sandbox-windows-amd64.zip
cd ephemeral-sandbox-windows-amd64
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\bin\start-sandbox-windows-docker-gateway.ps1
```

More details: [Windows setup](docs/windows-setup.md).

### Console / Desktop UI

The UI lives in the separate
[Ephemeral Sandbox Console](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-console)
repository. Start the gateway above, then point the console at:

- socket: `127.0.0.1:7878`
- Linux/macOS token: `$HOME/.ephemeral-sandbox/gateway.token`
- Windows token: `$HOME\.ephemeral-sandbox\gateway.token`

The console serves the browser UI at `http://127.0.0.1:7880`.

## Choose an interface

| Interface | Best for | Start with |
|---|---|---|
| CLI | Operators, scripts, and local development | `sandbox-manager-cli help` |
| MCP | Coding agents and MCP-compatible clients | `bin/setup-codex-mcp` |

The CLI and MCP interfaces use three focused tool groups:

- **Management** creates, inspects, exports, and destroys sandboxes.
- **Runtime** runs commands and reads or changes files inside a sandbox.
- **Observability** inspects health, events, resources, and filesystem layers.

Each MCP server exposes one tool group:

```sh
sandbox-mcp --set management
sandbox-mcp --set runtime
sandbox-mcp --set observability
```

## How it works

1. **Share a stable base.** LayerStack keeps the project history available to
   every workspace session.
2. **Work in isolation.** Each agent gets its own writable workspace and
   execution boundary.
3. **Publish safely.** Ephemeral Sandbox checks concurrent changes before
   publishing the complete resolved change set, or publishes nothing.

For the deeper design, see the
[architecture overview](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-docs/blob/main/architecture/00-foundations/01-system-overview.md).

## Documentation

- [Documentation](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-docs)
  covers the overview, CLI, MCP, and architecture.
- [External tests and benchmarks](https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-test)
  live in their own repository.
- Repository-local notes cover [configuration](config/README.md),
  [Linux setup](docs/linux-setup.md), [macOS setup](docs/macos-setup.md),
  [Windows setup](docs/windows-setup.md),
  [daemon HTTP](docs/daemon-http/README.md), and
  [maintainer boundaries](docs/maintainer-architecture.md).
- Run `sandbox-manager-cli help`, `sandbox-runtime-cli --sandbox-id ID help`,
  or `sandbox-observability-cli help` for installed command syntax.

## Community

Ask questions and share feedback in the
[Ephemeral AI Lab Discord](https://discord.com/invite/WdDJ3tru9).

## How to contribute

Focused fixes, documentation improvements, and tests are welcome. Before
opening a pull request, run:

```sh
cargo fmt --check
cargo clippy --all-targets
cargo test
```

Keep the change focused and describe what it changes, why it is needed, and how
you verified it in the pull request.

## License

Ephemeral Sandbox is available under the [Apache License 2.0](LICENSE).
