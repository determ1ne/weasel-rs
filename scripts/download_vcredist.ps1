#Requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$destination = Join-Path $projectRoot 'artifacts\vcredist'
$previousSecurityProtocol = [Net.ServicePointManager]::SecurityProtocol
$temporaryFile = $null

try {
    [Net.ServicePointManager]::SecurityProtocol =
        $previousSecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $null = New-Item -ItemType Directory -Path $destination -Force
    foreach ($architecture in @('x86', 'x64')) {
        $filename = "vc_redist.$architecture.exe"
        $temporaryFile = Join-Path ([IO.Path]::GetTempPath()) (
            'weasel-rs-vcredist-{0}.exe' -f [Guid]::NewGuid().ToString('N')
        )
        $url = "https://aka.ms/vc14/$filename"
        Write-Host "Downloading $url"
        Invoke-WebRequest -Uri $url -OutFile $temporaryFile `
            -UseBasicParsing -TimeoutSec 300 -MaximumRedirection 10
        $signature = Get-AuthenticodeSignature -LiteralPath $temporaryFile
        if ($signature.Status -ne 'Valid' -or
            $null -eq $signature.SignerCertificate -or
            $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation(?:,|$)') {
            throw "Invalid Microsoft signature on $filename ($($signature.Status))."
        }
        Copy-Item -LiteralPath $temporaryFile -Destination (Join-Path $destination $filename) -Force
        Remove-Item -LiteralPath $temporaryFile -Force
        $temporaryFile = $null
        Write-Host "Saved $filename (signature verified)."
    }
} catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
} finally {
    [Net.ServicePointManager]::SecurityProtocol = $previousSecurityProtocol
    if ($null -ne $temporaryFile -and (Test-Path -LiteralPath $temporaryFile)) {
        Remove-Item -LiteralPath $temporaryFile -Force
    }
}
