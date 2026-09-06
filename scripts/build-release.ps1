#Requires -Version 5.1
[CmdletBinding()]
param()

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
    $missingTargets = @($buildTargets | Where-Object { $_ -notin $installedTargets })
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
    Write-Host 'Release builds completed:'
    Write-Host "  x64 components: $(Join-Path $targetDirectory 'x86_64-pc-windows-msvc\release')"
    Write-Host "  x86 TIP:        $(Join-Path $targetDirectory 'i686-pc-windows-msvc\release\weasel_tip.dll')"
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    Pop-Location
}
