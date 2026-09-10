#Requires -Version 5.1
[CmdletBinding()]
param(
    # Build an uncompressed installer that installs all components without a selection page.
    [switch]$Dev,
    [switch]$Mini
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot

try {
    $compiler = (Get-Command makensis -CommandType Application -ErrorAction Stop).Source
    # Read the same Cargo version used by the native version resources.
    $manifest = Get-Content -LiteralPath (Join-Path $projectRoot 'tip\Cargo.toml') -Raw
    $versionMatch = [regex]::Match($manifest, '(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"\s*$')
    if (-not $versionMatch.Success) {
        throw 'TIP Cargo.toml must have a numeric major.minor.patch version.'
    }
    $version = $versionMatch.Groups[1].Value

    $requiredFiles = @(
        'weasel.json',
        'target\x86_64-pc-windows-msvc\release\weasel-broker.exe',
        'target\x86_64-pc-windows-msvc\release\weasel-server.exe',
        'target\x86_64-pc-windows-msvc\release\weasel-renderer.exe',
        'target\x86_64-pc-windows-msvc\release\weasel_tip.dll',
        'target\i686-pc-windows-msvc\release\weasel_tip.dll',
        'artifacts\librime\dist\lib\rime.dll',
        'assets\rime-data\default.yaml',
        'artifacts\librime\dist\lib\rime.pdb',
        'target\x86_64-pc-windows-msvc\release\weasel_broker.pdb',
        'target\x86_64-pc-windows-msvc\release\weasel_server.pdb',
        'target\x86_64-pc-windows-msvc\release\weasel_renderer.pdb',
        'target\x86_64-pc-windows-msvc\release\weasel_tip.pdb',
        'target\i686-pc-windows-msvc\release\weasel_tip.pdb',
        'artifacts\vcredist\vc_redist.x86.exe',
        'artifacts\vcredist\vc_redist.x64.exe',
        'assets\weasel.ico',
        'server\src\styles-LICENSE.txt',
        'LICENSE',
        'THIRD-PARTY-LICENSES.txt',
        'THIRD-PARTY-GPL-3.0.txt',
        'installer\weasel-rs.nsi'
        'scripts\launch-broker.ps1'
    )
    foreach ($theme in @('ten', 'eleven', 'abc', 'void')) {
        foreach ($extension in @('dll', 'pdb')) {
            $requiredFiles += "target\x86_64-pc-windows-msvc\release\weasel_theme_$theme.$extension"
        }
    }
    foreach ($theme in @('abc', 'eleven')) {
        $requiredFiles += "target\x86_64-pc-windows-msvc\release\themes\weasel_theme_$theme.settings.json"
    }
    if ($Mini) {
        $requiredFiles = @($requiredFiles | Where-Object { $_ -notlike '*.pdb' })
        $requiredFiles += 'scripts\download-runtime.ps1'
    }
    foreach ($relativePath in $requiredFiles) {
        if (-not (Test-Path -LiteralPath (Join-Path $projectRoot $relativePath) -PathType Leaf)) {
            throw "Missing $relativePath. Run scripts\build-release.ps1, scripts\download_librime.ps1 and scripts\download_vcredist.ps1 first."
        }
    }

    $outputDirectory = Join-Path $projectRoot 'artifacts\installer'
    $null = New-Item -ItemType Directory -Path $outputDirectory -Force
    $buildSuffix = if ($Dev) { '-dev' } else { '' }
    if ($Mini) { $buildSuffix += '-mini' }
    $outputFile = Join-Path $outputDirectory "Weasel-RS-$version-x64$buildSuffix-setup.exe"
    $runtimeDefinitions = @()
    foreach ($architecture in @('x86', 'x64')) {
        $runtimePath = Join-Path $projectRoot "artifacts\vcredist\vc_redist.$architecture.exe"
        $signature = Get-AuthenticodeSignature -LiteralPath $runtimePath
        if ($signature.Status -ne 'Valid' -or
            $null -eq $signature.SignerCertificate -or
            $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation(?:,|$)') {
            throw "Invalid Microsoft signature: $runtimePath"
        }
        $info = [Diagnostics.FileVersionInfo]::GetVersionInfo($runtimePath)
        if ($info.FileMajorPart -ne 14) {
            throw "Expected VC14 redistributable: $runtimePath"
        }
        $runtimeVersion = '{0}.{1}.{2}.{3}' -f $info.FileMajorPart, $info.FileMinorPart, $info.FileBuildPart, $info.FilePrivatePart
        $runtimeDefinitions += "/DVC_$($architecture.ToUpperInvariant())_VERSION=$runtimeVersion"
        Write-Host "Required $architecture VC++ runtime: $runtimeVersion"
    }
    if ($Mini) {
        Write-Host 'Mini: VC++ runtimes downloaded during installation; debug symbols excluded.'
    } else {
        Write-Host 'Full: VC++ runtimes bundled; debug symbols available as an optional component.'
    }
    # WASM artifacts are optional; NSIS recursively includes them when present.
    Write-Host 'Optional WASM modules: artifacts\theme-wasm (no build is triggered).'
    $arguments = @(
        '/V3', '/INPUTCHARSET', 'UTF8',
        "/DPROJECT_ROOT=$projectRoot",
        "/DPRODUCT_VERSION=$version",
        "/DOUTPUT_FILE=$outputFile",
        (Join-Path $projectRoot 'installer\weasel-rs.nsi')
    )
    $buildDefinitions = @()
    if ($Mini) { $buildDefinitions += '/DMINI_INSTALLER' }
    if ($Dev) {
        $buildDefinitions += '/DDEV_INSTALLER'
        Write-Host 'Dev installer: compression disabled; component selection skipped (all components selected).'
    }
    & $compiler @runtimeDefinitions @buildDefinitions @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "NSIS compilation failed (exit code $LASTEXITCODE)."
    }
    if (-not (Test-Path -LiteralPath $outputFile -PathType Leaf)) {
        throw 'NSIS did not produce the expected installer.'
    }
    Write-Host "Installer built: $outputFile"
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
}
