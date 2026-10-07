#Requires -Version 5.1
[CmdletBinding()]
param(
    # Build an uncompressed installer that installs all components without a selection page.
    [switch]$Dev,
    [switch]$Mini,
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture = 'x64'
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

    $nativeTarget = if ($Architecture -eq 'arm64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
    $serverTarget = if ($Architecture -eq 'arm64') { 'arm64ec-pc-windows-msvc' } else { $nativeTarget }
    $nativeRelease = "target\$nativeTarget\release"
    $serverRelease = "target\$serverTarget\release"
    $tip64Release = if ($Architecture -eq 'arm64') { 'target\arm64x-tip\release' } else { $nativeRelease }
    $tip64Directory = if ($Architecture -eq 'arm64') { 'arm64' } else { 'x64' }
    $runtimeArchitectures = if ($Architecture -eq 'arm64') { @('x86', 'x64', 'arm64') } else { @('x86', 'x64') }

    $requiredFiles = @(
        'weasel.json',
        "$nativeRelease\weasel-broker.exe",
        "artifacts\winsparkle\$Architecture\WinSparkle.dll",
        'artifacts\winsparkle\WinSparkle-LICENSE.txt',
        'artifacts\winsparkle\WinSparkle-Expat-LICENSE.txt',
        "$serverRelease\weasel-server.exe",
        "$nativeRelease\weasel-renderer.exe",
        "$nativeRelease\uiaccess\weasel-renderer.exe",
        "$nativeRelease\uiaccess\weasel_renderer.pdb",
        'scripts\sign-renderer.ps1',
        "$nativeRelease\weasel-settings.exe",
        "$tip64Release\weasel_tip.dll",
        'target\i686-pc-windows-msvc\release\weasel_tip.dll',
        'artifacts\librime\dist\lib\rime.dll',
        'assets\rime-data\default.yaml',
        'artifacts\librime\dist\lib\rime.pdb',
        "$nativeRelease\weasel_broker.pdb",
        "$serverRelease\weasel_server.pdb",
        "$nativeRelease\weasel_renderer.pdb",
        "$nativeRelease\weasel_settings.pdb",
        "$tip64Release\weasel_tip.pdb",
        'target\i686-pc-windows-msvc\release\weasel_tip.pdb',
        'assets\weasel.ico',
        'server\src\styles-LICENSE.txt',
        'LICENSE',
        'THIRD-PARTY-LICENSES.txt',
        'THIRD-PARTY-GPL-3.0.txt',
        'settings\LICENSE-NOTICE.txt',
        'installer\weasel-rs.nsi',
        'scripts\launch-broker.ps1'
    )
    foreach ($runtimeArchitecture in $runtimeArchitectures) {
        $requiredFiles += "artifacts\vcredist\vc_redist.$runtimeArchitecture.exe"
    }
    if ($Architecture -eq 'arm64') {
        $requiredFiles += @(
            "$tip64Release\weasel_tip_arm64.dll",
            "$tip64Release\weasel_tip_arm64ec.dll",
            "$tip64Release\weasel_tip_arm64.pdb",
            "$tip64Release\weasel_tip_arm64ec.pdb"
        )
    }
    foreach ($theme in @('ten', 'eleven', 'abc', 'void')) {
        foreach ($extension in @('dll', 'pdb')) {
            $requiredFiles += "$nativeRelease\weasel_theme_$theme.$extension"
        }
    }
    foreach ($theme in @('abc', 'eleven')) {
        $requiredFiles += "$nativeRelease\themes\weasel_theme_$theme.settings.json"
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
    # Expand recursive payloads at build time, never scan the installed directory.
    # Install and removal macros are generated from the same file set so nested
    # payloads cannot silently outlive an uninstall or a failed fresh install.
    $payloadInclude = Join-Path $outputDirectory 'payload-files.nsh'
    $payloadLines = [Collections.Generic.List[string]]::new()
    $payloadInstallPaths = [Collections.Generic.List[string]]::new()
    $payloadRemovalPaths = [Collections.Generic.List[string]]::new()
    $payloadDestinations = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($payload in @(
        @{ Macro = 'RimePayload'; RemoveMacro = 'RemoveRimePayload'; ValidateMacro = 'ValidateRimePayloadRemoval'; Source = 'assets\rime-data'; Target = 'rime-data' },
        @{ Macro = 'WasmPayload'; RemoveMacro = 'RemoveWasmPayload'; ValidateMacro = 'ValidateWasmPayloadRemoval'; Source = 'artifacts\theme-wasm'; Target = 'theme-wasm' }
    )) {
        $files = @()
        $payloadLines.Add('!macro ' + $payload.Macro)
        $sourceDirectory = Join-Path $projectRoot $payload.Source
        if (Test-Path -LiteralPath $sourceDirectory) {
            $entries = @(Get-Item -LiteralPath $sourceDirectory) + @(Get-ChildItem -LiteralPath $sourceDirectory -Recurse -Force)
            if ($entries | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }) {
                throw "Payload contains a reparse point: $sourceDirectory"
            }
            $files = @(Get-ChildItem -LiteralPath $sourceDirectory -Recurse -File | Sort-Object FullName)
            foreach ($file in $files) {
                if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Payload contains a reparse point: $($file.FullName)" }
                $relative = $file.FullName.Substring($sourceDirectory.Length + 1)
                if ($relative -match '[\$"\r\n]' -or $file.FullName -match '[\$"\r\n]') { throw "Unsupported payload filename: $relative" }
                $parent = Split-Path -Parent $relative
                $destination = $payload.Target
                if ($parent) { $destination += '\' + $parent }
                $installedPath = $payload.Target + '\' + $relative
                if (-not $payloadDestinations.Add($installedPath)) {
                    throw "Duplicate payload destination: $installedPath"
                }
                $payloadInstallPaths.Add($installedPath)
                $payloadLines.Add('SetOutPath "$INSTDIR\' + $destination + '"')
                $payloadLines.Add('!insertmacro ManagedFile "' + $file.FullName + '" "' + $file.Name + '"')
            }
        }
        $payloadLines.Add('!macroend')

        $payloadLines.Add('!macro ' + $payload.RemoveMacro)
        $directories = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        $null = $directories.Add($payload.Target)
        foreach ($file in $files) {
            $relative = $file.FullName.Substring($sourceDirectory.Length + 1)
            $installedPath = $payload.Target + '\' + $relative
            $payloadRemovalPaths.Add($installedPath)
            $payloadLines.Add('Delete /REBOOTOK "$INSTDIR\' + $installedPath + '"')
            $parent = Split-Path -Parent $installedPath
            while ($parent) {
                $null = $directories.Add($parent)
                if ($parent -eq $payload.Target) { break }
                $parent = Split-Path -Parent $parent
            }
        }
        foreach ($directory in $directories | Sort-Object @{ Expression = { ($_ -split '[\\/]').Count }; Descending = $true }, @{ Expression = { $_ }; Descending = $true }) {
            $payloadLines.Add('RMDir /REBOOTOK "$INSTDIR\' + $directory + '"')
        }
        $payloadLines.Add('!macroend')

        $payloadLines.Add('!macro ' + $payload.ValidateMacro)
        foreach ($directory in $directories | Sort-Object @{ Expression = { ($_ -split '[\\/]').Count }; Descending = $false }, @{ Expression = { $_ }; Descending = $false }) {
            $payloadLines.Add('!insertmacro AssertSafeUninstallDirectory "$INSTDIR\' + $directory + '"')
        }
        $payloadLines.Add('!macroend')
    }
    $payloadDifference = @(Compare-Object -ReferenceObject $payloadInstallPaths -DifferenceObject $payloadRemovalPaths)
    if ($payloadDifference.Count -ne 0) {
        throw "Generated payload install/removal sets differ: $($payloadDifference | Out-String)"
    }
    [IO.File]::WriteAllLines($payloadInclude, $payloadLines, [Text.UTF8Encoding]::new($false))
    $buildSuffix = if ($Dev) { '-dev' } else { '' }
    if ($Mini) { $buildSuffix += '-mini' }
    $outputFile = Join-Path $outputDirectory "Weasel-RS-$version-$Architecture$buildSuffix-setup.exe"
    $runtimeDefinitions = @()
    foreach ($runtimeArchitecture in $runtimeArchitectures) {
        $runtimePath = Join-Path $projectRoot "artifacts\vcredist\vc_redist.$runtimeArchitecture.exe"
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
        $runtimeDefinitions += "/DVC_$($runtimeArchitecture.ToUpperInvariant())_VERSION=$runtimeVersion"
        Write-Host "Required $runtimeArchitecture VC++ runtime: $runtimeVersion"
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
        "/DPRODUCT_ARCH=$Architecture",
        "/DOUTPUT_FILE=$outputFile",
        "/DPAYLOAD_INCLUDE=$payloadInclude",
        "/DNATIVE_RELEASE=$(Join-Path $projectRoot $nativeRelease)",
        "/DSERVER_RELEASE=$(Join-Path $projectRoot $serverRelease)",
        "/DTIP64_RELEASE=$(Join-Path $projectRoot $tip64Release)",
        "/DTIP64_DIR=$tip64Directory",
        "/DWINSPARKLE_DLL=$(Join-Path $projectRoot "artifacts\winsparkle\$Architecture\WinSparkle.dll")",
        (Join-Path $projectRoot 'installer\weasel-rs.nsi')
    )
    $buildDefinitions = @()
    if ($Mini) { $buildDefinitions += '/DMINI_INSTALLER' }
    if ($Architecture -eq 'arm64') { $buildDefinitions += '/DARM64_INSTALLER' }
    if (Test-Path -LiteralPath (Join-Path $projectRoot "$nativeRelease\weasel_theme_wasm.dll") -PathType Leaf) {
        $buildDefinitions += '/DHAVE_WASM_THEME'
    }
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
