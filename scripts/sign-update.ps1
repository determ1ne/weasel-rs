#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Installer,
    [Parameter(Mandatory)][string]$PublicKey
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$tool = Join-Path $projectRoot 'artifacts\winsparkle\winsparkle-tool.exe'
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
$key = $env:WINSPARKLE_PRIVATE_KEY
if ([string]::IsNullOrWhiteSpace($key)) { throw 'WINSPARKLE_PRIVATE_KEY is missing.' }
if (-not (Test-Path -LiteralPath $tool -PathType Leaf)) { throw 'Run scripts/download_winsparkle.ps1 first.' }
$temporaryDirectory = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$keyFile = Join-Path $temporaryDirectory ("weasel-winsparkle-{0}.key" -f [guid]::NewGuid().ToString('N'))
try {
    [IO.File]::WriteAllText($keyFile, $key.Trim(), [Text.UTF8Encoding]::new($false))
    $keyInfo = @(& $tool public-key --private-key-file $keyFile)
    if ($LASTEXITCODE -ne 0) { throw 'Unable to read WinSparkle signing key.' }
    $publicMatch = [regex]::Match(($keyInfo -join "`n"), 'Public key:\s*([A-Za-z0-9+/=]+)')
    if (-not $publicMatch.Success -or $publicMatch.Groups[1].Value -cne $PublicKey) {
        throw 'The WinSparkle signing key does not match the public key embedded in the broker.'
    }
    $signature = (@(& $tool sign --private-key-file $keyFile $installerPath) -join '').Trim()
    if ($LASTEXITCODE -ne 0) { throw 'WinSparkle signing failed.' }
    try { $signatureBytes = [Convert]::FromBase64String($signature) }
    catch { throw 'WinSparkle returned an invalid base64 signature.' }
    if ($signatureBytes.Length -ne 64) { throw 'WinSparkle returned an invalid Ed25519 signature length.' }
    & $tool verify --public-key $PublicKey --signature $signature $installerPath
    if ($LASTEXITCODE -ne 0) { throw 'WinSparkle signature verification failed.' }
    [IO.File]::WriteAllText("$installerPath.edSignature", "$signature`n", [Text.UTF8Encoding]::new($false))
    Write-Host "Signed and verified $([IO.Path]::GetFileName($installerPath))."
} finally {
    if (Test-Path -LiteralPath $keyFile) { Remove-Item -LiteralPath $keyFile -Force }
}
