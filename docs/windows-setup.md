# Windows Setup

This guide runs the Ephemeral Sandbox core gateway and Docker-backed sandboxes
from a native Windows checkout. The sandbox daemon still runs inside Linux
containers, so Windows only builds the host gateway and CLI binaries; the daemon
binary is cross-built for Linux amd64.

## Requirements

- Windows 11 with Docker Desktop using the Linux container engine.

Source builds additionally need:

- Visual Studio 2022 Build Tools with the C++ build tools workload.
- Rust through rustup.
- Zig and `cargo-zigbuild` for the Linux musl daemon package.
- Git for Windows. Its `usr\bin\sh.exe` is used by the packaging task.
- `make.exe`, for example from `ezwinports.make`.
- Node.js 24 when also running the browser console.

PowerShell blocks `npm.ps1` on many machines. Use `npm.cmd` in the commands
below unless your execution policy already permits npm's PowerShell shim.

## Binary Release

Download and start the Windows amd64 release:

```powershell
curl.exe -LO https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox/releases/latest/download/ephemeral-sandbox-windows-amd64.zip
tar.exe -xf ephemeral-sandbox-windows-amd64.zip
cd ephemeral-sandbox-windows-amd64
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\bin\start-sandbox-windows-docker-gateway.ps1
```

The launcher writes the gateway token to:

```text
$HOME\.ephemeral-sandbox\gateway.token
```

Binary releases do not require Rust, Cargo, Zig, or `cargo-zigbuild`.

## Build The Core From Source

Start in the core checkout:

```powershell
git clone https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox.git
cd ephemeral-sandbox
```

Build the Windows host binaries from a VS toolchain environment:

```powershell
cmd.exe /d /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 -host_arch=x64 && cargo build --locked -p sandbox-gateway --bin sandbox-gateway -p sandbox-cli"
```

Build the Linux daemon that Docker sandboxes upload into each container:

```powershell
cargo install cargo-zigbuild
cargo build --locked -p xtask --bin xtask

$env:Path = "C:\Program Files\Git\usr\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
target\debug\xtask.exe package --builder zigbuild --target x86_64-unknown-linux-musl
```

If Zig or `make.exe` is not on `PATH`, add their install directories before
running the package command. The package command writes:

```text
dist\sandbox-daemon-linux-amd64
```

## Start The Docker Gateway

Pull a small test image and start the gateway with the Windows amd64 config:

```powershell
docker pull alpine:3.20

$env:SANDBOX_GATEWAY_AUTH_TOKEN = [guid]::NewGuid().ToString("N")
Set-Content target\windows-gateway.token $env:SANDBOX_GATEWAY_AUTH_TOKEN
target\debug\sandbox-gateway.exe serve `
  --backend docker `
  --config-yaml config\windows-amd64.yml `
  --gateway-endpoint npipe://./pipe/ephemeral-sandbox-gateway `
  --auth-token $env:SANDBOX_GATEWAY_AUTH_TOKEN `
  --pid-file target\windows-gateway.pid
```

Keep this process running. Open a second PowerShell window in the same checkout
for the CLI checks.

## Verify The CLIs

Use a small smoke workspace first. Binding the core checkout after building can
copy several gigabytes of `target` artifacts into the shared base cache.

```powershell
$env:SANDBOX_GATEWAY_AUTH_TOKEN = Get-Content target\windows-gateway.token
$smoke = Resolve-Path ..\ephemeral-sandbox-windows-smoke -ErrorAction SilentlyContinue
if (-not $smoke) {
  New-Item -ItemType Directory ..\ephemeral-sandbox-windows-smoke\src -Force | Out-Null
  Set-Content ..\ephemeral-sandbox-windows-smoke\README.txt "hello from Windows smoke workspace"
  Set-Content ..\ephemeral-sandbox-windows-smoke\src\main.txt "sandbox smoke file"
  $smoke = Resolve-Path ..\ephemeral-sandbox-windows-smoke
}

target\debug\sandbox-manager-cli.exe `
  --gateway-endpoint npipe://./pipe/ephemeral-sandbox-gateway `
  --gateway-auth-token $env:SANDBOX_GATEWAY_AUTH_TOKEN `
  list_docker_images

$created = target\debug\sandbox-manager-cli.exe `
  --gateway-endpoint npipe://./pipe/ephemeral-sandbox-gateway `
  --gateway-auth-token $env:SANDBOX_GATEWAY_AUTH_TOKEN `
  create_sandbox `
  --image alpine:3.20 `
  --workspace-bind-root $smoke

$sandboxId = ($created | ConvertFrom-Json).id

target\debug\sandbox-runtime-cli.exe `
  --gateway-endpoint npipe://./pipe/ephemeral-sandbox-gateway `
  --gateway-auth-token $env:SANDBOX_GATEWAY_AUTH_TOKEN `
  --sandbox-id $sandboxId `
  exec_command "pwd && cat README.txt && ls src"

target\debug\sandbox-observability-cli.exe `
  --gateway-endpoint npipe://./pipe/ephemeral-sandbox-gateway `
  --gateway-auth-token $env:SANDBOX_GATEWAY_AUTH_TOKEN `
  snapshot --sandbox-id $sandboxId
```

Expected command output includes `/workspace`, `hello from Windows smoke
workspace`, and `main.txt`.

## Run The Web Console

The browser UI lives in the separate console repository.
The console currently uses TCP, so stop the named-pipe gateway and restart the
packaged launcher with
`-GatewaySocket tcp://127.0.0.1:7878` before starting the console.

```powershell
cd ..
git clone https://github.com/Ephemeral-AI-Lab/ephemeral-sandbox-console.git
cd ephemeral-sandbox-console\web

npm.cmd ci
npm.cmd run build
cd ..

cmd.exe /d /c "call C:\BuildTools\Common7\Tools\VsDevCmd.bat -arch=x64 -host_arch=x64 && cargo build --locked -p sandbox-console --bin sandbox-console"

target\debug\sandbox-console.exe `
  --bind 127.0.0.1:7880 `
  --gateway-socket 127.0.0.1:7878 `
  --gateway-auth-token $env:SANDBOX_GATEWAY_AUTH_TOKEN `
  --gateway-start-root ..\ephemeral-sandbox `
  --assets web\dist
```

Open `http://127.0.0.1:7880`. The dashboard should show the smoke sandbox as
ready. Open it, use the Terminal tab, and run:

```sh
pwd && cat README.txt && ls src
```

The terminal ledger should show `ok`, `/workspace`, the README text, and
`main.txt`.

The console includes a fixed **Start/reload backend** button. It uses
`--gateway-start-root`, `EPHEMERAL_SANDBOX_ROOT`, or a sibling
`ephemeral-sandbox` checkout to find the built
`target\debug\sandbox-gateway.exe`, starts it with the same socket and auth
token when needed, waits for a readiness check, and then reloads the UI.

## Troubleshooting

- `link.exe` is missing: run builds through `VsDevCmd.bat`, or start a
  Developer PowerShell for VS 2022.
- `npm.ps1 cannot be loaded`: use `npm.cmd`.
- `daemon.server.socket_path: must be an absolute path`: use
  `config\windows-amd64.yml` and a revision that validates in-container paths
  as Unix paths.
- `layer-stack io error: Access is denied` while creating the shared base:
  use a revision with Windows directory-fsync no-op support.
- Shared-base target contains backslashes, such as `/eos/layer-stack\base`:
  use a revision that builds container mount paths with Unix separators.
- Sandbox creation is slow or memory-heavy: do not use a checkout containing a
  large `target` directory as the first smoke workspace. Use a small workspace
  or remove generated build artifacts before binding the project root.
