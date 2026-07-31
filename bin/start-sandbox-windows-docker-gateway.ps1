[CmdletBinding()]
param(
    [string]$GatewaySocket = $(if ($env:SANDBOX_GATEWAY_SOCKET) { $env:SANDBOX_GATEWAY_SOCKET } else { "npipe://./pipe/ephemeral-sandbox-gateway" }),
    [string]$ConfigYaml = $(if ($env:SANDBOX_GATEWAY_CONFIG_YAML) { $env:SANDBOX_GATEWAY_CONFIG_YAML } else { "" }),
    [string]$PidFile = $(if ($env:SANDBOX_GATEWAY_PID_FILE) { $env:SANDBOX_GATEWAY_PID_FILE } else { Join-Path $env:TEMP "eos-gateway-windows.pid" }),
    [string]$LogPath = $(if ($env:SANDBOX_GATEWAY_LOG) { $env:SANDBOX_GATEWAY_LOG } else { Join-Path $env:TEMP "eos-gateway-windows.log" })
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Fail($Message) {
    Write-Error $Message
    exit 1
}

function Resolve-ExistingPath($Candidates, $Description) {
    foreach ($candidate in $Candidates) {
        if (Test-Path -LiteralPath $candidate) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    Fail "$Description not found. Checked: $($Candidates -join ', ')"
}

function Quote-ProcessArgument($Value) {
    $text = [string]$Value
    if ($text -match '[\s"]') {
        return '"' + ($text -replace '"', '\"') + '"'
    }
    return $text
}

$scriptDir = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptDir "..")).Path

if (-not $ConfigYaml) {
    $ConfigYaml = Join-Path $repoRoot "config\windows-amd64.yml"
}
$ConfigYaml = Resolve-ExistingPath @($ConfigYaml) "gateway config"

$gatewayExe = Resolve-ExistingPath @(
    (Join-Path $repoRoot "bin\sandbox-gateway.exe"),
    (Join-Path $repoRoot "target\release\sandbox-gateway.exe"),
    (Join-Path $repoRoot "target\debug\sandbox-gateway.exe")
) "sandbox-gateway.exe"

$daemonArtifact = Resolve-ExistingPath @(
    (Join-Path $repoRoot "dist\sandbox-daemon-linux-amd64")
) "Linux amd64 sandbox daemon artifact"

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    Fail "docker not found; install Docker Desktop and use Linux containers"
}

docker info *> $null
if ($LASTEXITCODE -ne 0) {
    Fail "docker daemon is not reachable; start Docker Desktop or fix Docker permissions"
}

$tokenDir = Join-Path $HOME ".ephemeral-sandbox"
$tokenPath = Join-Path $tokenDir "gateway.token"
New-Item -ItemType Directory -Force -Path $tokenDir | Out-Null

if ($env:SANDBOX_GATEWAY_AUTH_TOKEN) {
    $authToken = $env:SANDBOX_GATEWAY_AUTH_TOKEN
} elseif (Test-Path -LiteralPath $tokenPath) {
    $authToken = (Get-Content -LiteralPath $tokenPath -TotalCount 1).Trim()
} else {
    $authToken = [guid]::NewGuid().ToString("N")
}
Set-Content -LiteralPath $tokenPath -Value $authToken -NoNewline
$env:SANDBOX_GATEWAY_AUTH_TOKEN = $authToken

if (Test-Path -LiteralPath $PidFile) {
    $oldPidText = (Get-Content -LiteralPath $PidFile -TotalCount 1 -ErrorAction SilentlyContinue)
    $oldPid = 0
    if ([int]::TryParse($oldPidText, [ref]$oldPid)) {
        $oldProcess = Get-Process -Id $oldPid -ErrorAction SilentlyContinue
        if ($oldProcess -and $oldProcess.ProcessName -like "sandbox-gateway*") {
            Write-Host "stopping existing sandbox-gateway pid $oldPid"
            Stop-Process -Id $oldPid -Force
        }
    }
    Remove-Item -LiteralPath $PidFile -Force -ErrorAction SilentlyContinue
}

$logDir = Split-Path -Parent $LogPath
$pidDir = Split-Path -Parent $PidFile
if ($logDir) { New-Item -ItemType Directory -Force -Path $logDir | Out-Null }
if ($pidDir) { New-Item -ItemType Directory -Force -Path $pidDir | Out-Null }

$errorLogPath = if ($LogPath.EndsWith(".log", [StringComparison]::OrdinalIgnoreCase)) {
    $LogPath.Substring(0, $LogPath.Length - 4) + ".err.log"
} else {
    "$LogPath.err"
}

$gatewayArgs = @(
    "serve",
    "--backend", "docker",
    "--config-yaml", $ConfigYaml,
    "--gateway-endpoint", $GatewaySocket,
    "--auth-token", $authToken,
    "--pid-file", $PidFile
)
$argumentLine = ($gatewayArgs | ForEach-Object { Quote-ProcessArgument $_ }) -join " "

$process = Start-Process `
    -FilePath $gatewayExe `
    -ArgumentList $argumentLine `
    -WorkingDirectory $repoRoot `
    -RedirectStandardOutput $LogPath `
    -RedirectStandardError $errorLogPath `
    -WindowStyle Hidden `
    -PassThru

for ($i = 0; $i -lt 30; $i++) {
    if ($process.HasExited) {
        Write-Error "sandbox-gateway exited during startup; see logs: $LogPath $errorLogPath"
        Get-Content -LiteralPath $LogPath -Tail 20 -ErrorAction SilentlyContinue
        Get-Content -LiteralPath $errorLogPath -Tail 20 -ErrorAction SilentlyContinue
        exit 1
    }
    if (Test-Path -LiteralPath $PidFile) {
        break
    }
    Start-Sleep -Milliseconds 100
}

Write-Host "sandbox-gateway starting in background via pid $($process.Id)"
Write-Host "pid file: $PidFile"
Write-Host "endpoint: $GatewaySocket"
Write-Host "auth token file: $tokenPath"
Write-Host "log: $LogPath"
Write-Host "error log: $errorLogPath"
Write-Host "daemon artifact: $daemonArtifact"
