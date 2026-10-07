#Requires -Version 7.0
[CmdletBinding()]
param(
    [string]$TargetDirectory = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target'),
    [switch]$Dev,
    [switch]$SkipRustBuild,
    [switch]$UseCurrentMsvcEnvironment
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'msvc-arm64.ps1')
$configuration = if ($Dev) { 'debug' } else { 'release' }
$cargoProfile = if ($Dev) { @() } else { @('--release') }
$output = Join-Path $TargetDirectory "arm64x-tip\$configuration"
$previousEntry = [Environment]::GetEnvironmentVariable('WEASEL_TIP_ENTRY_DLL', 'Process')
$previousProcessEnvironment = if ($UseCurrentMsvcEnvironment) { $null } else { Save-ProcessEnvironment }

function Resolve-Tool([string]$Name) {
    $command = Get-Command $Name -CommandType Application -ErrorAction Stop | Select-Object -First 1
    if (-not $command.Source) { throw "Unable to resolve $Name." }
    $command.Source
}

try {
    if (-not $UseCurrentMsvcEnvironment) {
        Import-MsvcArm64Environment
    }
    $cargo = Resolve-Tool cargo
    $cl = Resolve-Tool cl
    $link = Resolve-Tool link
    $null = New-Item -ItemType Directory -Force -Path $output
    $env:WEASEL_TIP_ENTRY_DLL = 'weasel_tip.dll'

    foreach ($target in @('aarch64-pc-windows-msvc', 'arm64ec-pc-windows-msvc')) {
        if (-not $SkipRustBuild) {
            & $cargo build @cargoProfile --locked -p weasel-tip --target $target --target-dir $TargetDirectory
            if ($LASTEXITCODE -ne 0) { throw "TIP build failed for $target." }
        }
        $source = Join-Path $TargetDirectory "$target\$configuration"
        $suffix = if ($target -eq 'aarch64-pc-windows-msvc') { 'arm64' } else { 'arm64ec' }
        Copy-Item -LiteralPath (Join-Path $source 'weasel_tip.dll') -Destination (Join-Path $output "weasel_tip_$suffix.dll") -Force
        Copy-Item -LiteralPath (Join-Path $source 'weasel_tip.pdb') -Destination (Join-Path $output "weasel_tip_$suffix.pdb") -Force
    }

    $sourceDirectory = Join-Path $projectRoot 'tip\arm64x'
    & $cl /nologo /c /Fo"$output\empty_arm64.obj" (Join-Path $sourceDirectory 'empty.cpp')
    if ($LASTEXITCODE -ne 0) { throw 'Unable to compile the ARM64 forwarder object. Run from an ARM64 MSVC developer environment.' }
    & $cl /nologo /arm64EC /c /Fo"$output\empty_arm64ec.obj" (Join-Path $sourceDirectory 'empty.cpp')
    if ($LASTEXITCODE -ne 0) { throw 'Unable to compile the ARM64EC forwarder object.' }

    $arm64Def = Join-Path $sourceDirectory 'weasel-tip-arm64.def'
    $arm64ecDef = Join-Path $sourceDirectory 'weasel-tip-arm64ec.def'
    # These temporary import libraries intentionally contain COM exports: the
    # ARM64X linker consumes them to synthesize architecture-specific forwarders.
    & $link /lib /ignore:4104 /machine:arm64 "/def:$arm64Def" "/out:$output\weasel_tip_arm64.lib"
    if ($LASTEXITCODE -ne 0) { throw 'Unable to create the ARM64 forwarder import library.' }
    & $link /lib /ignore:4104 /machine:x64 "/def:$arm64ecDef" "/out:$output\weasel_tip_arm64ec.lib"
    if ($LASTEXITCODE -ne 0) { throw 'Unable to create the ARM64EC forwarder import library.' }
    & $link /dll /noentry /debug /ignore:4104 /machine:arm64x "/defArm64Native:$arm64Def" "/def:$arm64ecDef" `
        "$output\empty_arm64.obj" "$output\empty_arm64ec.obj" `
        "$output\weasel_tip_arm64.lib" "$output\weasel_tip_arm64ec.lib" `
        "/out:$output\weasel_tip.dll" "/pdb:$output\weasel_tip.pdb"
    if ($LASTEXITCODE -ne 0) { throw 'Unable to link the ARM64X TIP forwarder.' }

    foreach ($file in @('weasel_tip.dll', 'weasel_tip_arm64.dll', 'weasel_tip_arm64ec.dll')) {
        if (-not (Test-Path -LiteralPath (Join-Path $output $file) -PathType Leaf)) {
            throw "Missing ARM64X TIP output: $file"
        }
    }
    Write-Host "ARM64X TIP staged in $output"
} finally {
    [Environment]::SetEnvironmentVariable('WEASEL_TIP_ENTRY_DLL', $previousEntry, 'Process')
    if ($previousProcessEnvironment) {
        Restore-ProcessEnvironment $previousProcessEnvironment
    }
}
