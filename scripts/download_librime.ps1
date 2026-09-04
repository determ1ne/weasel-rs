#Requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$projectRoot = Split-Path -Parent $PSScriptRoot
$destination = Join-Path $projectRoot 'artifacts\librime'
$downloadUrl = 'https://github.com/rime/librime/releases/download/1.17.0/rime-33e7814-Windows-msvc-x64.7z'
$archivePath = Join-Path ([IO.Path]::GetTempPath()) (
    'weasel-rs-librime-{0}.7z' -f [Guid]::NewGuid().ToString('N')
)
$previousSecurityProtocol = [Net.ServicePointManager]::SecurityProtocol

try {
    $sevenZip = Get-Command 7z, 7za, 7zz -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -eq $sevenZip) {
        throw '7-Zip is required. Install it and add 7z, 7za or 7zz to PATH.'
    }

    # Windows PowerShell 5.1 may otherwise use an older TLS default.
    [Net.ServicePointManager]::SecurityProtocol =
        $previousSecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    Write-Host "Downloading $downloadUrl"
    Invoke-WebRequest -Uri $downloadUrl -OutFile $archivePath `
        -UseBasicParsing -TimeoutSec 300 -MaximumRedirection 10

    Write-Host "Extracting to $destination"
    # Preserve archive paths; replace matching files without deleting unrelated files.
    & $sevenZip.Source x $archivePath "-o$destination" -y -aoa
    if ($LASTEXITCODE -ne 0) {
        throw "7-Zip extraction failed (exit code $LASTEXITCODE)."
    }
    Write-Host 'librime 1.17.0 (MSVC x64) download completed.'
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    [Net.ServicePointManager]::SecurityProtocol = $previousSecurityProtocol
    if (Test-Path -LiteralPath $archivePath) {
        Remove-Item -LiteralPath $archivePath -Force
    }
}
