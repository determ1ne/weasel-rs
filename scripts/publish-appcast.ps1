#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Tag,
    [string]$ProjectName = $env:WINSPARKLE_PAGES_PROJECT,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($Tag -cnotmatch '^v(\d+\.\d+\.\d+)$') { throw 'Tag must be vMAJOR.MINOR.PATCH.' }
$version = $Matches[1]
$repository = 'determ1ne/weasel-rs'
$publicKey = if ([string]::IsNullOrWhiteSpace($env:WINSPARKLE_PUBLIC_KEY)) {
    'GAmmGQQoLRtAPyTj3s2gtK9NfVnlcqPHaGJbDF3Zc9E='
} else {
    $env:WINSPARKLE_PUBLIC_KEY
}
$feedUrl = if ([string]::IsNullOrWhiteSpace($env:WINSPARKLE_APPCAST_URL)) {
    'https://rimers.sigsegv.top/appcast.xml'
} else {
    $env:WINSPARKLE_APPCAST_URL
}
if ($feedUrl -notmatch '^https://[^/]+/appcast\.xml$') {
    throw 'WINSPARKLE_APPCAST_URL must be an HTTPS URL ending in /appcast.xml.'
}
$headers = @{ 'User-Agent' = 'weasel-rs-appcast-publisher'; 'Accept' = 'application/vnd.github+json' }
$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/tags/$Tag" -Headers $headers
if ($release.draft -or $release.prerelease -or -not $release.published_at) {
    throw 'Appcast may only point to a published, non-prerelease GitHub Release.'
}
$latest = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/latest" -Headers $headers
if ($latest.tag_name -cne $Tag) { throw "The latest published release is $($latest.tag_name), not $Tag." }

$platforms = @(
    @{ Architecture = 'x64'; SparkleOs = 'windows-x64' },
    @{ Architecture = 'arm64'; SparkleOs = 'windows-arm64' }
)
$updates = foreach ($platform in $platforms) {
    $assetName = "Weasel-RS-$version-$($platform.Architecture)-mini-setup.exe"
    $installer = @($release.assets | Where-Object { $_.name -ceq $assetName })
    $signatureAsset = @($release.assets | Where-Object { $_.name -ceq "$assetName.edSignature" })
    if ($installer.Count -ne 1 -or $signatureAsset.Count -ne 1) {
        throw "Release must contain exactly one $assetName and $assetName.edSignature."
    }
    if ($installer[0].size -le 0 -or $installer[0].browser_download_url -notmatch '^https://') {
        throw "Mini installer asset metadata is invalid: $assetName"
    }
    $signatureContent = (Invoke-WebRequest -Uri $signatureAsset[0].browser_download_url -Headers $headers).Content
    $signature = if ($signatureContent -is [byte[]]) {
        [Text.Encoding]::UTF8.GetString($signatureContent).Trim()
    } else {
        ([string]$signatureContent).Trim()
    }
    try { $signatureBytes = [Convert]::FromBase64String($signature) }
    catch { throw "Release signature is not base64: $assetName" }
    if ($signatureBytes.Length -ne 64) { throw "Release signature is not Ed25519 (64 bytes): $assetName" }
    [pscustomobject]@{
        Installer = $installer[0]
        Signature = $signature
        SparkleOs = $platform.SparkleOs
        AssetName = $assetName
    }
}

$root = Split-Path -Parent $PSScriptRoot
$tool = Join-Path $root 'artifacts\winsparkle\winsparkle-tool.exe'
if (-not (Test-Path -LiteralPath $tool -PathType Leaf)) {
    & (Join-Path $PSScriptRoot 'download_winsparkle.ps1')
}
$temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) ("weasel-appcast-{0}" -f [guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $temporaryDirectory
try {
    foreach ($update in $updates) {
        $temporaryInstaller = Join-Path $temporaryDirectory $update.AssetName
        Invoke-WebRequest -Uri $update.Installer.browser_download_url -Headers $headers -OutFile $temporaryInstaller -TimeoutSec 600
        if ((Get-Item -LiteralPath $temporaryInstaller).Length -ne $update.Installer.size) {
            throw "Downloaded Mini installer length differs from release metadata: $($update.AssetName)"
        }
        & $tool verify --public-key $publicKey --signature $update.Signature $temporaryInstaller
        if ($LASTEXITCODE -ne 0) { throw "Published Mini installer does not match its WinSparkle signature: $($update.AssetName)" }
    }
} finally {
    if (Test-Path -LiteralPath $temporaryDirectory) { Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force }
}

$outputDirectory = Join-Path $root 'artifacts\appcast'
$null = New-Item -ItemType Directory -Path $outputDirectory -Force
$otherFiles = @(Get-ChildItem -LiteralPath $outputDirectory -Force | Where-Object { $_.Name -cne 'appcast.xml' })
if ($otherFiles.Count -gt 0) { throw 'Appcast output directory contains unexpected files; refusing to publish them.' }
$output = Join-Path $outputDirectory 'appcast.xml'
$settings = [System.Xml.XmlWriterSettings]::new()
$settings.Indent = $true
$settings.Encoding = [Text.UTF8Encoding]::new($false)
$namespace = 'http://www.andymatuschak.org/xml-namespaces/sparkle'
$writer = [System.Xml.XmlWriter]::Create($output, $settings)
try {
    $writer.WriteStartDocument()
    $writer.WriteStartElement('rss')
    $writer.WriteAttributeString('version', '2.0')
    $writer.WriteAttributeString('xmlns', 'sparkle', $null, $namespace)
    $writer.WriteStartElement('channel')
    $writer.WriteElementString('title', '小狼毫RS 更新')
    $writer.WriteElementString('description', '小狼毫RS 稳定版更新')
    $writer.WriteElementString('language', 'zh-CN')
    $writer.WriteStartElement('item')
    $writer.WriteElementString('title', "小狼毫RS $version")
    $writer.WriteElementString('sparkle', 'version', $namespace, $version)
    # Temporary bridge: older brokers without a Windows 10 supportedOS manifest
    # are reported as Windows 8 by VerifyVersionInfoW and would miss this update.
    # $writer.WriteElementString('sparkle', 'minimumSystemVersion', $namespace, '10.0.17763')
    $writer.WriteElementString('sparkle', 'releaseNotesLink', $namespace, $release.html_url)
    $date = ([datetimeoffset]$release.published_at).ToUniversalTime().ToString(
        "ddd, dd MMM yyyy HH:mm:ss 'GMT'", [Globalization.CultureInfo]::InvariantCulture)
    $writer.WriteElementString('pubDate', $date)
    foreach ($update in $updates) {
        $writer.WriteStartElement('enclosure')
        $writer.WriteAttributeString('url', $update.Installer.browser_download_url)
        $writer.WriteAttributeString('length', [string]$update.Installer.size)
        $writer.WriteAttributeString('type', 'application/octet-stream')
        $writer.WriteAttributeString('sparkle', 'os', $namespace, $update.SparkleOs)
        $writer.WriteAttributeString('sparkle', 'edSignature', $namespace, $update.Signature)
        $writer.WriteEndElement()
    }
    $writer.WriteEndElement()
    $writer.WriteEndElement()
    $writer.WriteEndElement()
    $writer.WriteEndDocument()
} finally { $writer.Dispose() }

if ($DryRun) {
    Write-Host "Validated and generated $output (not deployed)."
    return
}
if (-not $ProjectName) { throw 'Set WINSPARKLE_PAGES_PROJECT or pass -ProjectName.' }
if (-not (Get-Command wrangler -CommandType Application -ErrorAction SilentlyContinue)) {
    throw 'Install Wrangler and add it to PATH before deploying.'
}
& wrangler pages deploy $outputDirectory --project-name $ProjectName --branch main
if ($LASTEXITCODE -ne 0) { throw 'Cloudflare Pages deployment failed.' }
Write-Host "Published $feedUrl for $Tag."
