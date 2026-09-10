#Requires -Version 5.1
[CmdletBinding()]
param(
    # Skip the native WASM backend and guest modules; retain existing artifacts.
    [switch]$SkipThemeWasm
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$targetDirectory = Join-Path $projectRoot 'target'
$buildTargets = @('x86_64-pc-windows-msvc', 'i686-pc-windows-msvc')

Push-Location -LiteralPath $projectRoot
try {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'This build script requires Windows and the MSVC build tools.'
    }
    $cargoCommand = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
    $rustupCommand = (Get-Command rustup -CommandType Application -ErrorAction Stop).Source
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

    foreach ($buildTarget in $buildTargets) {
        $buildArguments = @(
            'build', '--release', '--locked',
            '--target', $buildTarget,
            '--target-dir', $targetDirectory
        )
        if ($buildTarget -eq 'x86_64-pc-windows-msvc') {
            $buildArguments += '--workspace'
            if ($SkipThemeWasm) {
                $buildArguments += @('--exclude', 'weasel-theme-wasm')
            }
        } else {
            # Only the in-process TIP needs x86 host compatibility.
            $buildArguments += @('-p', 'weasel-tip')
        }
        Write-Host "Building release: $buildTarget"
        & $cargoCommand @buildArguments
        if ($LASTEXITCODE -ne 0) {
            throw "Release build failed for $buildTarget (exit code $LASTEXITCODE)."
        }
    }

    Copy-Item -LiteralPath (Join-Path $projectRoot 'weasel.json') -Destination (Join-Path $targetDirectory 'x86_64-pc-windows-msvc\release\weasel.json')
    # Match the installed/portable DLL layout next to the renderer.
    $releaseDirectory = Join-Path $targetDirectory 'x86_64-pc-windows-msvc\release'
    $themeDirectory = Join-Path $releaseDirectory 'themes'
    $null = New-Item -ItemType Directory -Path $themeDirectory -Force
    $nodeCommand = (Get-Command node -CommandType Application -ErrorAction Stop).Source
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
                    $npmCommand = (Get-Command npm.cmd -CommandType Application -ErrorAction Stop).Source
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
            } finally {
                Pop-Location
            }
        }
        Write-Host "WASM modules: $wasmArtifacts"
    }
    Write-Host 'Release builds completed:'
    Write-Host "  x64 components: $(Join-Path $targetDirectory 'x86_64-pc-windows-msvc\release')"
    Write-Host "  x86 TIP:        $(Join-Path $targetDirectory 'i686-pc-windows-msvc\release\weasel_tip.dll')"
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    Pop-Location
}
