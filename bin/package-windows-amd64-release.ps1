[CmdletBinding()]
param(
    [string]$PackageName = $(if ($env:SANDBOX_RELEASE_PACKAGE_NAME) { $env:SANDBOX_RELEASE_PACKAGE_NAME } else { "ephemeral-sandbox-windows-amd64" }),
    [string]$OutDir = $(if ($env:SANDBOX_RELEASE_OUT_DIR) { $env:SANDBOX_RELEASE_OUT_DIR } else { "" }),
    [ValidateSet("release", "debug")]
    [string]$Profile = "release",
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail($Message) {
    Write-Error $Message
    exit 1
}

function Require-Command($Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        Fail "$Name not found"
    }
}

$scriptDir = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptDir "..")).Path
if (-not $OutDir) {
    $OutDir = Join-Path $repoRoot "dist\release"
} elseif (-not [System.IO.Path]::IsPathRooted($OutDir)) {
    $OutDir = Join-Path $repoRoot $OutDir
}

$stageDir = Join-Path $OutDir $PackageName
$archive = Join-Path $OutDir "$PackageName.zip"
$hashFile = "$archive.sha256"
$targetDir = Join-Path (Join-Path $repoRoot "target") $Profile

Require-Command cargo

if (-not $SkipBuild) {
    Push-Location $repoRoot
    try {
        $profileArg = @()
        if ($Profile -eq "release") {
            $profileArg = @("--release")
        }

        cargo build --locked @profileArg -p sandbox-gateway --bin sandbox-gateway
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        cargo build --locked @profileArg -p sandbox-cli --no-default-features --features manager --bin sandbox-manager-cli
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        cargo build --locked @profileArg -p sandbox-cli --no-default-features --features runtime --bin sandbox-runtime-cli
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        cargo build --locked @profileArg -p sandbox-cli --no-default-features --features observability --bin sandbox-observability-cli
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally {
        Pop-Location
    }
}

$requiredFiles = @(
    (Join-Path $targetDir "sandbox-gateway.exe"),
    (Join-Path $targetDir "sandbox-manager-cli.exe"),
    (Join-Path $targetDir "sandbox-runtime-cli.exe"),
    (Join-Path $targetDir "sandbox-observability-cli.exe"),
    (Join-Path $repoRoot "dist\sandbox-daemon-linux-amd64"),
    (Join-Path $repoRoot "config\windows-amd64.yml"),
    (Join-Path $repoRoot "bin\start-sandbox-windows-docker-gateway.ps1"),
    (Join-Path $repoRoot "LICENSE")
)

foreach ($file in $requiredFiles) {
    if (-not (Test-Path -LiteralPath $file)) {
        Fail "required release input not found: $file"
    }
}

Remove-Item -LiteralPath $stageDir, $archive, $hashFile -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "bin") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "config") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "dist") | Out-Null

Copy-Item -LiteralPath (Join-Path $targetDir "sandbox-gateway.exe") -Destination (Join-Path $stageDir "bin\sandbox-gateway.exe")
Copy-Item -LiteralPath (Join-Path $targetDir "sandbox-manager-cli.exe") -Destination (Join-Path $stageDir "bin\sandbox-manager-cli.exe")
Copy-Item -LiteralPath (Join-Path $targetDir "sandbox-runtime-cli.exe") -Destination (Join-Path $stageDir "bin\sandbox-runtime-cli.exe")
Copy-Item -LiteralPath (Join-Path $targetDir "sandbox-observability-cli.exe") -Destination (Join-Path $stageDir "bin\sandbox-observability-cli.exe")
Copy-Item -LiteralPath (Join-Path $repoRoot "bin\start-sandbox-windows-docker-gateway.ps1") -Destination (Join-Path $stageDir "bin\start-sandbox-windows-docker-gateway.ps1")
Copy-Item -LiteralPath (Join-Path $repoRoot "config\windows-amd64.yml") -Destination (Join-Path $stageDir "config\windows-amd64.yml")
Copy-Item -LiteralPath (Join-Path $repoRoot "dist\sandbox-daemon-linux-amd64") -Destination (Join-Path $stageDir "dist\sandbox-daemon-linux-amd64")
Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE") -Destination (Join-Path $stageDir "LICENSE")

@'
# Ephemeral Sandbox Windows amd64

Docker Desktop must be running with the Linux container engine.

Start the gateway:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\bin\start-sandbox-windows-docker-gateway.ps1
```

Use the gateway from another PowerShell window:

```powershell
$env:SANDBOX_GATEWAY_AUTH_TOKEN = Get-Content "$HOME\.ephemeral-sandbox\gateway.token"
$env:SANDBOX_IMAGE = "alpine:3.20"
.\bin\sandbox-manager-cli.exe --gateway-endpoint npipe://./pipe/ephemeral-sandbox-gateway --gateway-auth-token $env:SANDBOX_GATEWAY_AUTH_TOKEN list_docker_images
```
'@ | Set-Content -LiteralPath (Join-Path $stageDir "INSTALL.md") -Encoding UTF8

Compress-Archive -LiteralPath $stageDir -DestinationPath $archive -Force
$hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $(Split-Path -Leaf $archive)" | Set-Content -LiteralPath $hashFile -Encoding ASCII

Write-Host "release archive: $archive"
Get-Item -LiteralPath $archive | Select-Object FullName, Length
