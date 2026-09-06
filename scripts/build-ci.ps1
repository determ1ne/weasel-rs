#Requires -Version 7.0
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot

Push-Location -LiteralPath $projectRoot
try {
    $metadata = & cargo metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) { throw 'Unable to read Cargo metadata.' }
    $metadata = $metadata | ConvertFrom-Json
    $members = @($metadata.packages | Where-Object { $_.id -in $metadata.workspace_members })
    $versions = @($members.version | Sort-Object -Unique)
    if ($versions.Count -ne 1) { throw 'Workspace package versions must match.' }
    $version = $versions[0]

    & cargo test --workspace --locked
    if ($LASTEXITCODE -ne 0) { throw 'Workspace tests failed.' }

    foreach ($script in @('download_librime.ps1', 'download_vcredist.ps1', 'build-release.ps1', 'build-installer.ps1')) {
        & pwsh -NoProfile -File (Join-Path $PSScriptRoot $script)
        if ($LASTEXITCODE -ne 0) { throw "$script failed (exit code $LASTEXITCODE)." }
    }

    $installer = Join-Path $projectRoot "artifacts\installer\Weasel-RS-$version-x64-setup.exe"
    $hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    $filename = Split-Path -Leaf $installer
    Set-Content -LiteralPath "$installer.sha256" -Value "$hash  $filename" -Encoding utf8
    if ($env:GITHUB_OUTPUT) {
        Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "version=$version"
    }
} finally {
    Pop-Location
}
