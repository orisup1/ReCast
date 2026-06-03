# recast - Windows installer (PowerShell)
#
# Mirrors the Makefile for Windows hosts. Default target is `deploy`
# (clean + build + install). Use `-Target service` to also register a
# Scheduled Task that starts recast at logon.
#
# The binary is self-contained: dictionaries are embedded at compile time
# (`include_str!` in src/main.rs), so the executable runs identically from
# any working directory — no wrapper or data dir is required.
#
# Usage:
#   .\deploy.ps1                        # = .\deploy.ps1 -Target deploy
#   .\deploy.ps1 -Target build
#   .\deploy.ps1 -Target install
#   .\deploy.ps1 -Target service
#   .\deploy.ps1 -Target service-uninstall
#   .\deploy.ps1 -Target uninstall
#   .\deploy.ps1 -Target clean
#   .\deploy.ps1 -Target run
#   .\deploy.ps1 -Target help
#
# Optional:
#   .\deploy.ps1 -Prefix "C:\Tools\recast"   # custom install dir for the bin

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet(
        'build','clean','rebuild',
        'install','uninstall','deploy','run',
        'service','service-uninstall','help'
    )]
    [string]$Target = 'deploy',

    [string]$Prefix = (Join-Path $env:USERPROFILE '.local\bin')
)

$ErrorActionPreference = 'Stop'

$BinName  = 'recast.exe'
$BinSrc   = Join-Path 'target\release' $BinName
$BinDir   = $Prefix
$BinDst   = Join-Path $BinDir $BinName
$TaskName = 'recast'

function Write-Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

function Invoke-Build {
    Write-Step 'cargo build --release'
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
}

function Invoke-Clean {
    Write-Step 'cargo clean'
    & cargo clean
    if ($LASTEXITCODE -ne 0) { throw "cargo clean failed (exit $LASTEXITCODE)" }
}

function Invoke-Rebuild {
    Invoke-Clean
    Invoke-Build
}

function Invoke-Install {
    Invoke-Build

    if (-not (Test-Path $BinDir)) { New-Item -ItemType Directory -Path $BinDir | Out-Null }

    Write-Step "Installing to $BinDst"
    Copy-Item -Force -Path $BinSrc -Destination $BinDst

    Write-Host ""
    Write-Host "Installed:"
    Write-Host "  $BinDst"
    Write-Host ""
    Write-Host "Make sure $BinDir is on your PATH, then run: recast"
    Write-Host "(rdev needs no special permissions on Windows.)"
}

function Invoke-Uninstall {
    if (Test-Path $BinDst) {
        Remove-Item -Force $BinDst
        Write-Host "Removed $BinDst"
    } else {
        Write-Host "Nothing to remove at $BinDst"
    }
}

function Invoke-Deploy {
    Invoke-Clean
    Invoke-Install
}

function Invoke-ServiceInstall {
    Invoke-Install

    Write-Step "Registering Scheduled Task '$TaskName' (runs at logon)"

    # Tear down any prior version so we always end up with a fresh definition.
    if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    }

    $action    = New-ScheduledTaskAction -Execute $BinDst
    $trigger   = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
    $principal = New-ScheduledTaskPrincipal `
                    -UserId $env:USERNAME `
                    -LogonType Interactive `
                    -RunLevel Limited
    $settings  = New-ScheduledTaskSettingsSet `
                    -AllowStartIfOnBatteries `
                    -DontStopIfGoingOnBatteries `
                    -RestartCount 3 `
                    -RestartInterval (New-TimeSpan -Minutes 1) `
                    -ExecutionTimeLimit (New-TimeSpan -Hours 0)

    Register-ScheduledTask `
        -TaskName  $TaskName `
        -Action    $action `
        -Trigger   $trigger `
        -Principal $principal `
        -Settings  $settings | Out-Null

    Start-ScheduledTask -TaskName $TaskName

    Write-Host ""
    Write-Host "Scheduled Task '$TaskName' registered (runs at logon) and started."
    Write-Host "  status: Get-ScheduledTask -TaskName $TaskName"
    Write-Host "  remove: .\deploy.ps1 -Target service-uninstall"
}

function Invoke-ServiceUninstall {
    if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
        try { Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue } catch {}
        Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
        Write-Host "Scheduled Task '$TaskName' removed."
    } else {
        Write-Host "No Scheduled Task '$TaskName' found; nothing to remove."
    }
}

function Invoke-Run {
    Invoke-Build
    Write-Step 'cargo run --release'
    & cargo run --release
}

function Invoke-Help {
    Write-Host @"
recast deploy.ps1 (Windows)

Targets:
  build              cargo build --release
  clean              cargo clean
  rebuild            clean + build
  install            build + copy binary to -Prefix
  uninstall          remove installed bin
  deploy             clean + build + install (default)
  service            install + register Scheduled Task (logon autostart)
  service-uninstall  remove the Scheduled Task
  run                cargo run --release
  help               show this message

Parameters:
  -Target <name>     target to run (default: deploy)
  -Prefix <path>     install dir for the bin (default: %USERPROFILE%\.local\bin)

Current values:
  Prefix = $Prefix
  BinDst = $BinDst

The binary is self-contained — dictionaries are embedded at compile time, so
it runs identically from any working directory.

For Linux / macOS hosts, use the Makefile in this directory instead.
"@
}

switch ($Target) {
    'build'             { Invoke-Build }
    'clean'             { Invoke-Clean }
    'rebuild'           { Invoke-Rebuild }
    'install'           { Invoke-Install }
    'uninstall'         { Invoke-Uninstall }
    'deploy'            { Invoke-Deploy }
    'service'           { Invoke-ServiceInstall }
    'service-uninstall' { Invoke-ServiceUninstall }
    'run'               { Invoke-Run }
    'help'              { Invoke-Help }
}
