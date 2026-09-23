#Requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$version = '0.9.4'
$expected = '6037df37fc263bd1650a1c4949681a9d40ffe991d01f35892a406cb5d103c976'
$directory = Join-Path (Split-Path -Parent $PSScriptRoot) 'artifacts\winsparkle'
$archive = Join-Path $directory "WinSparkle-$version.zip"
$temporary = Join-Path $directory "WinSparkle-$version.download"

try {
    $null = New-Item -ItemType Directory -Force -Path $directory
    if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
        $url = "https://github.com/vslavik/winsparkle/releases/download/v$version/WinSparkle-$version.zip"
        Invoke-WebRequest -Uri $url -OutFile $temporary -TimeoutSec 300 -MaximumRedirection 10
        $actual = (Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) { throw "WinSparkle SHA-256 mismatch: expected $expected, actual $actual" }
        Move-Item -LiteralPath $temporary -Destination $archive
    }
    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "WinSparkle SHA-256 mismatch: expected $expected, actual $actual" }

    Add-Type -AssemblyName System.IO.Compression
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        $entries = @{
            "WinSparkle-$version/x64/Release/WinSparkle.dll" = 'WinSparkle.dll'
            "WinSparkle-$version/bin/winsparkle-tool.exe" = 'winsparkle-tool.exe'
            "WinSparkle-$version/COPYING" = 'WinSparkle-LICENSE.txt'
            "WinSparkle-$version/COPYING.expat" = 'WinSparkle-Expat-LICENSE.txt'
        }
        foreach ($entryName in $entries.Keys) {
            $entry = $zip.GetEntry($entryName)
            if ($null -eq $entry) { throw "Missing WinSparkle archive entry: $entryName" }
            $destination = Join-Path $directory $entries[$entryName]
            $source = $entry.Open()
            $output = [IO.File]::Create($destination)
            try { $source.CopyTo($output) }
            finally { $output.Dispose(); $source.Dispose() }
        }
    } finally { $zip.Dispose() }
    Write-Host "WinSparkle ${version}: SHA-256 verified; x64 DLL, signing tool and license staged."
} finally {
    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
}
