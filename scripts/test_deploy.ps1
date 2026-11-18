# Run the installer from another project, with Cargo confined to a disposable fixture.
$ErrorActionPreference = 'Stop'
$fixture = Join-Path ([System.IO.Path]::GetTempPath()) ([guid]::NewGuid())
$project = Join-Path $fixture 'ReCast project'
$caller = Join-Path $fixture 'caller project'
New-Item -ItemType Directory -Path $project, $caller | Out-Null
try {
    $script = Join-Path $project 'deploy.ps1'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../deploy.ps1') -Destination $script
    function cargo {
        if ((Get-Location).Path -ne $project) { throw "Cargo ran outside ReCast" }
        if ($args[0] -eq 'build') {
            New-Item -Force -ItemType Directory -Path 'target/release' | Out-Null
            Set-Content -LiteralPath 'target/release/recast.exe' -Value 'fixture'
        }
        $global:LASTEXITCODE = 0
    }
    Push-Location -LiteralPath $caller
    try {
        & $script -Target deploy -Prefix 'relative prefix'
        if ((Get-Location).Path -ne $caller) { throw "Installer changed caller location" }
        if ((Get-Content -LiteralPath 'relative prefix/recast.exe') -ne 'fixture') {
            throw "Installer did not preserve caller-relative prefix"
        }
    } finally {
        Pop-Location
    }
} finally {
    Remove-Item -LiteralPath $fixture -Recurse -Force
}
