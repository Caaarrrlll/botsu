# Runs botsu locally with settings from .env.
#   ./scripts/run.ps1            debug build, log level from .env
#   ./scripts/run.ps1 -Release   release build (what the Docker image runs), RUST_LOG=info
#   ./scripts/run.ps1 -Example api_spike -- <args>   a botsu-fluxer example (spikes) with the same settings
param(
    [switch]$Release,
    [string]$Example,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$envFile = Join-Path $root '.env'

if (-not (Test-Path $envFile)) {
    throw ".env not found. Copy .env.example to .env and fill in the tokens."
}

# Same rules as Docker's --env-file: KEY=value per line, # comments, no quote handling.
foreach ($line in Get-Content $envFile) {
    if ($line -match '^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=(.*)$') {
        Set-Item "env:$($Matches[1])" $Matches[2].Trim()
    }
}

$cargoArgs = if ($Example) { @('run', '-p', 'botsu-fluxer', '--example', $Example) } else { @('run', '-p', 'botsu') }
if ($Release) {
    $cargoArgs += '--release'
    $env:RUST_LOG = 'info'
}

$extra = @($Rest | Where-Object { $_ -ne '--' })
if ($extra.Count -gt 0) {
    $cargoArgs += '--'
    $cargoArgs += $extra
}

Push-Location $root
try {
    cargo @cargoArgs
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
