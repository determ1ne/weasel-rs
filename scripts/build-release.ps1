#Requires -Version 5.1
[CmdletBinding()]
param(
    # Keep release outputs/optimizations, but enable Rust incremental compilation.
    [switch]$Dev,
    # Skip the native WASM backend and guest modules; retain existing artifacts.
    [switch]$SkipThemeWasm,
    [ValidateSet('x64', 'arm64')]
    [string]$Architecture = 'x64'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-Application {
    param(
        [Parameter(Mandatory)]
        [string]$Name
    )

    # Get-Command may return both a version manager's real executable and its
    # shim. The invocation operator requires exactly one command path.
    $command = Get-Command $Name -CommandType Application -ErrorAction Stop |
        Select-Object -First 1
    if ($null -eq $command -or [string]::IsNullOrWhiteSpace($command.Source)) {
        throw "Unable to resolve application: $Name"
    }
    $command.Source
}

$projectRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'msvc-arm64.ps1')
$previousProcessEnvironment = if ($Architecture -eq 'arm64') { Save-ProcessEnvironment } else { $null }
$targetDirectory = Join-Path $projectRoot 'target'
$nativeTarget = if ($Architecture -eq 'arm64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
$serverTarget = if ($Architecture -eq 'arm64') { 'arm64ec-pc-windows-msvc' } else { $nativeTarget }
$buildTargets = @($nativeTarget, 'i686-pc-windows-msvc')
if ($Architecture -eq 'arm64') { $buildTargets += 'arm64ec-pc-windows-msvc' }
$previousCargoIncremental = [Environment]::GetEnvironmentVariable('CARGO_INCREMENTAL', 'Process')
$previousUiAccess = [Environment]::GetEnvironmentVariable('WEASEL_RENDERER_UIACCESS', 'Process')
$previousAppcastUrl = [Environment]::GetEnvironmentVariable('WINSPARKLE_APPCAST_URL', 'Process')
$previousPublicKey = [Environment]::GetEnvironmentVariable('WINSPARKLE_PUBLIC_KEY', 'Process')
$previousTipEntry = [Environment]::GetEnvironmentVariable('WEASEL_TIP_ENTRY_DLL', 'Process')

Push-Location -LiteralPath $projectRoot
try {
    if ([string]::IsNullOrWhiteSpace($previousAppcastUrl)) {
        $env:WINSPARKLE_APPCAST_URL = 'https://rimers.sigsegv.top/appcast.xml'
    }
    if ([string]::IsNullOrWhiteSpace($previousPublicKey)) {
        $env:WINSPARKLE_PUBLIC_KEY = 'GAmmGQQoLRtAPyTj3s2gtK9NfVnlcqPHaGJbDF3Zc9E='
    }
    $env:WEASEL_RENDERER_UIACCESS = '0'
    if ($Dev) {
        # Cargo already caches unchanged crates in target. Release builds normally
        # disable incremental compilation; opt in for repeated local edits. Keep
        # the same output paths so build-installer can consume these binaries.
        # This also applies to the separate Rust WASM guest builds below.
        $env:CARGO_INCREMENTAL = '1'
        Write-Host 'Development release build: Rust incremental compilation enabled; reusing target cache.'
    }
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'This build script requires Windows and the MSVC build tools.'
    }
    $cargoCommand = Resolve-Application 'cargo'
    $rustupCommand = Resolve-Application 'rustup'
    # Resolve JavaScript tools before importing VsDevCmd. Some version managers
    # only expose Node through user PATH entries that VsDevCmd does not retain.
    $nodeCommand = Resolve-Application 'node'
    $npmCommand = if ($SkipThemeWasm) { $null } else { Resolve-Application 'npm.cmd' }
    $installedTargets = @(& $rustupCommand target list --installed)
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to list installed Rust targets (exit code $LASTEXITCODE)."
    }
    $requiredTargets = @($buildTargets)
    if (-not $SkipThemeWasm) { $requiredTargets += 'wasm32-unknown-unknown' }
    $missingTargets = @($requiredTargets | Where-Object { $_ -notin $installedTargets })
    if ($missingTargets.Count -gt 0) {
        throw "Install the missing targets first: rustup target add $($missingTargets -join ' ')"
    }

    # Build the x86 in-process TIP before importing an ARM64 developer
    # environment; otherwise LIB/INCLUDE would point at ARM64 libraries.
    Remove-Item Env:\WEASEL_TIP_ENTRY_DLL -ErrorAction SilentlyContinue
    Write-Host 'Building x86 TIP.'
    & $cargoCommand build --release --locked -p weasel-tip `
        --target i686-pc-windows-msvc --target-dir $targetDirectory
    if ($LASTEXITCODE -ne 0) { throw 'x86 TIP build failed.' }

    if ($Architecture -eq 'arm64') {
        Import-MsvcArm64Environment
        # VsDevCmd prepends the architecture-specific compiler and linker.
        $cargoCommand = Resolve-Application 'cargo'
        $rustupCommand = Resolve-Application 'rustup'
        # npm-run launches `node` by name. Keep both resolved tool directories
        # visible to child processes even when the ARM64 developer environment
        # replaced the user portion of PATH.
        $javascriptToolDirectories = @(
            Split-Path -Parent $nodeCommand
            if ($npmCommand) { Split-Path -Parent $npmCommand }
        ) | Select-Object -Unique
        $pathEntries = @($env:Path -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        foreach ($directory in $javascriptToolDirectories) {
            if ($directory -notin $pathEntries) {
                $pathEntries = @($directory) + $pathEntries
            }
        }
        $env:Path = $pathEntries -join ';'
    }
    if ($Architecture -eq 'arm64') {
        $env:WEASEL_TIP_ENTRY_DLL = 'weasel_tip.dll'
    } else {
        Remove-Item Env:\WEASEL_TIP_ENTRY_DLL -ErrorAction SilentlyContinue
    }
    $nativeArguments = @(
        'build', '--release', '--locked', '--workspace',
        '--target', $nativeTarget, '--target-dir', $targetDirectory
    )
    if ($Architecture -eq 'arm64') { $nativeArguments += @('--exclude', 'weasel-server') }
    if ($SkipThemeWasm) { $nativeArguments += @('--exclude', 'weasel-theme-wasm') }
    Write-Host "Building $Architecture native release: $nativeTarget"
    & $cargoCommand @nativeArguments
    if ($LASTEXITCODE -ne 0) { throw "Native release build failed for $nativeTarget (exit code $LASTEXITCODE)." }

    if ($Architecture -eq 'arm64') {
        Write-Host 'Building ARM64EC server and TIP.'
        & $cargoCommand build --release --locked -p weasel-server -p weasel-tip `
            --target $serverTarget --target-dir $targetDirectory
        if ($LASTEXITCODE -ne 0) { throw 'ARM64EC build failed.' }
    }

    if ($Architecture -eq 'arm64') {
        & (Join-Path $PSScriptRoot 'build-arm64x-tip.ps1') -TargetDirectory $targetDirectory `
            -SkipRustBuild -UseCurrentMsvcEnvironment
        if ($LASTEXITCODE -ne 0) { throw 'ARM64X TIP build failed.' }
    }

    Copy-Item -LiteralPath (Join-Path $projectRoot 'weasel.json') -Destination (Join-Path $targetDirectory "$nativeTarget\release\weasel.json")
    # Match the installed/portable DLL layout next to the renderer.
    $releaseDirectory = Join-Path $targetDirectory "$nativeTarget\release"
    # Keep Cargo's primary output ordinary. Build the opt-in variant first, copy
    # its matching symbols, then rebuild ordinary so Cargo's cache stays honest.
    $uiAccessDirectory = Join-Path $releaseDirectory 'uiaccess'
    $null = New-Item -ItemType Directory -Path $uiAccessDirectory -Force
    try {
        $env:WEASEL_RENDERER_UIACCESS = '1'
        & $cargoCommand build --release --locked -p weasel-renderer --target $nativeTarget --target-dir $targetDirectory
        if ($LASTEXITCODE -ne 0) { throw 'UIAccess renderer build failed.' }
        foreach ($file in @('weasel-renderer.exe', 'weasel_renderer.pdb')) {
            Copy-Item -LiteralPath (Join-Path $releaseDirectory $file) -Destination $uiAccessDirectory -Force
        }
    } finally {
        $env:WEASEL_RENDERER_UIACCESS = '0'
        & $cargoCommand build --release --locked -p weasel-renderer --target $nativeTarget --target-dir $targetDirectory
        if ($LASTEXITCODE -ne 0) { throw 'Ordinary renderer rebuild failed; do not package these outputs.' }
    }
    $themeDirectory = Join-Path $releaseDirectory 'themes'
    $null = New-Item -ItemType Directory -Path $themeDirectory -Force
    $metadataPackager = Join-Path $PSScriptRoot 'package-theme-metadata.mjs'
    foreach ($theme in @('abc', 'eleven')) {
        & $nodeCommand $metadataPackager --native (Join-Path $projectRoot "themes\$theme\src\config.json") (Join-Path $themeDirectory "weasel_theme_$theme.settings.json")
        if ($LASTEXITCODE -ne 0) { throw "Metadata packaging failed for $theme." }
    }
    foreach ($theme in @('ten', 'eleven', 'abc', 'void', 'wasm')) {
        if ($SkipThemeWasm -and $theme -eq 'wasm') { continue }
        foreach ($extension in @('dll', 'pdb')) {
            Copy-Item -LiteralPath (Join-Path $releaseDirectory "weasel_theme_$theme.$extension") -Destination $themeDirectory
        }
    }
    if (-not $SkipThemeWasm) {
        $wasmArtifacts = Join-Path $projectRoot 'artifacts\theme-wasm'
        $null = New-Item -ItemType Directory -Path $wasmArtifacts -Force
        # Rust cdylibs and AssemblyScript guests share the artifact directory.
        # SDK directories are deliberately excluded from guest discovery.
        $guestDirectories = @(Get-ChildItem -LiteralPath (Join-Path $projectRoot 'themes\wasm') -Directory |
            Where-Object { $_.Name -like 'theme-*' } | Sort-Object Name)
        foreach ($guest in $guestDirectories) {
            Write-Host "Building WASM theme: $($guest.Name)"
            Push-Location -LiteralPath $guest.FullName
            try {
                if (Test-Path -LiteralPath 'Cargo.toml' -PathType Leaf) {
                    $metadataJson = & $cargoCommand metadata --no-deps --format-version 1 --locked
                    if ($LASTEXITCODE -ne 0) { throw "Cargo metadata failed for $($guest.Name)." }
                    $metadata = ($metadataJson -join "`n") | ConvertFrom-Json
                    $manifestPath = (Join-Path $guest.FullName 'Cargo.toml').Replace('\', '/')
                    $package = @($metadata.packages | Where-Object { $_.manifest_path.Replace('\', '/') -eq $manifestPath })
                    if ($package.Count -ne 1) { throw "Cannot identify Rust guest package: $($guest.Name)." }
                    $libraries = @($package[0].targets | Where-Object { 'cdylib' -in $_.crate_types })
                    if ($libraries.Count -ne 1) { throw "Rust guest must declare exactly one cdylib: $($guest.Name)." }
                    $guestTarget = Join-Path $targetDirectory 'theme-wasm-rust'
                    & $cargoCommand build --release --locked --lib --target wasm32-unknown-unknown --target-dir $guestTarget
                    if ($LASTEXITCODE -ne 0) { throw "Rust WASM build failed for $($guest.Name)." }
                    $moduleName = $libraries[0].name.Replace('-', '_') + '.wasm'
                    $output = Join-Path $guestTarget "wasm32-unknown-unknown\release\$moduleName"
                } else {
                    & $npmCommand ci --no-audit --no-fund
                    if ($LASTEXITCODE -ne 0) { throw "npm ci failed for $($guest.Name)." }
                    & $npmCommand run build
                    if ($LASTEXITCODE -ne 0) { throw "WASM build failed for $($guest.Name)." }
                    $config = Get-Content -LiteralPath 'asconfig.json' -Raw | ConvertFrom-Json
                    $output = [IO.Path]::GetFullPath((Join-Path $guest.FullName $config.targets.release.outFile))
                }
                if (-not (Test-Path -LiteralPath $output -PathType Leaf) -or [IO.Path]::GetExtension($output) -ne '.wasm') {
                    throw "Missing WASM release output for $($guest.Name): $output"
                }
                if (Test-Path -LiteralPath (Join-Path $guest.FullName 'config.richschema.json') -PathType Leaf) {
                    & $nodeCommand $metadataPackager --wasm $output (Join-Path $guest.FullName 'config.json')
                    if ($LASTEXITCODE -ne 0) { throw "Metadata packaging failed for $($guest.Name)." }
                }
                Copy-Item -LiteralPath $output -Destination $wasmArtifacts -Force
                # 模块只能读取自己的同名 .assets 目录，安装包整体携带该目录。
                $guestAssets = Join-Path $guest.FullName 'assets'
                if (Test-Path -LiteralPath $guestAssets -PathType Container) {
                    $assetDestination = Join-Path $wasmArtifacts ([IO.Path]::GetFileNameWithoutExtension($output) + '.assets')
                    $null = New-Item -ItemType Directory -Path $assetDestination -Force
                    Get-ChildItem -LiteralPath $guestAssets | Copy-Item -Destination $assetDestination -Recurse -Force
                }
            } finally {
                Pop-Location
            }
        }
        Write-Host "WASM modules: $wasmArtifacts"
    }
    Write-Host 'Release builds completed:'
    Write-Host "  $Architecture components: $releaseDirectory"
    Write-Host "  server:          $(Join-Path $targetDirectory "$serverTarget\release\weasel-server.exe")"
    Write-Host "  x86 TIP:        $(Join-Path $targetDirectory 'i686-pc-windows-msvc\release\weasel_tip.dll')"
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    [Environment]::SetEnvironmentVariable('WINSPARKLE_APPCAST_URL', $previousAppcastUrl, 'Process')
    [Environment]::SetEnvironmentVariable('WINSPARKLE_PUBLIC_KEY', $previousPublicKey, 'Process')
    [Environment]::SetEnvironmentVariable('WEASEL_RENDERER_UIACCESS', $previousUiAccess, 'Process')
    [Environment]::SetEnvironmentVariable('WEASEL_TIP_ENTRY_DLL', $previousTipEntry, 'Process')
    if ($Dev) {
        [Environment]::SetEnvironmentVariable('CARGO_INCREMENTAL', $previousCargoIncremental, 'Process')
    }
    if ($previousProcessEnvironment) {
        Restore-ProcessEnvironment $previousProcessEnvironment
    }
    Pop-Location
}
