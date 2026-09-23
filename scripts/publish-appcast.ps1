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
$headers = @{ 'User-Agent' = 'weasel-rs-appcast-publisher'; 'Accept' = 'application/vnd.github+json' }
$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/tags/$Tag" -Headers $headers
if ($release.draft -or $release.prerelease -or -not $release.published_at) {
    throw 'Appcast may only point to a published, non-prerelease GitHub Release.'
}
$latest = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/latest" -Headers $headers
if ($latest.tag_name -cne $Tag) { throw "The latest published release is $($latest.tag_name), not $Tag." }

$assetName = "Weasel-RS-$version-x64-mini-setup.exe"
$installer = @($release.assets | Where-Object { $_.name -ceq $assetName })
$signatureAsset = @($release.assets | Where-Object { $_.name -ceq "$assetName.edSignature" })
if ($installer.Count -ne 1 -or $signatureAsset.Count -ne 1) {
    throw "Release must contain exactly one $assetName and $assetName.edSignature."
}
if ($installer[0].size -le 0 -or $installer[0].browser_download_url -notmatch '^https://') {
    throw 'Mini installer asset metadata is invalid.'
}
$signature = (Invoke-WebRequest -Uri $signatureAsset[0].browser_download_url -Headers $headers).Content.Trim()
try { $signatureBytes = [Convert]::FromBase64String($signature) }
catch { throw 'Release signature is not base64.' }
if ($signatureBytes.Length -ne 64) { throw 'Release signature is not Ed25519 (64 bytes).' }
if (-not $env:WINSPARKLE_PUBLIC_KEY) { throw 'Set WINSPARKLE_PUBLIC_KEY to the public key embedded in the broker.' }

$root = Split-Path -Parent $PSScriptRoot
$tool = Join-Path $root 'artifacts\winsparkle\winsparkle-tool.exe'
if (-not (Test-Path -LiteralPath $tool -PathType Leaf)) {
    & (Join-Path $PSScriptRoot 'download_winsparkle.ps1')
}
$temporaryInstaller = Join-Path ([IO.Path]::GetTempPath()) ("weasel-appcast-{0}.exe" -f [guid]::NewGuid().ToString('N'))
try {
    Invoke-WebRequest -Uri $installer[0].browser_download_url -Headers $headers -OutFile $temporaryInstaller -TimeoutSec 600
    if ((Get-Item -LiteralPath $temporaryInstaller).Length -ne $installer[0].size) {
        throw 'Downloaded Mini installer length differs from release metadata.'
    }
    & $tool verify --public-key $env:WINSPARKLE_PUBLIC_KEY --signature $signature $temporaryInstaller
    if ($LASTEXITCODE -ne 0) { throw 'Published Mini installer does not match its WinSparkle signature.' }
} finally {
    if (Test-Path -LiteralPath $temporaryInstaller) { Remove-Item -LiteralPath $temporaryInstaller -Force }
}

$feedUrl = $env:WINSPARKLE_APPCAST_URL
if (-not $feedUrl -or $feedUrl -notmatch '^https://[^/]+/appcast\.xml$') {
    throw 'Set WINSPARKLE_APPCAST_URL to the HTTPS Pages URL ending in /appcast.xml.'
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
    $writer.WriteElementString('sparkle', 'minimumSystemVersion', $namespace, '10.0.17763')
    $writer.WriteElementString('sparkle', 'releaseNotesLink', $namespace, $release.html_url)
    $date = ([datetimeoffset]$release.published_at).ToUniversalTime().ToString(
        "ddd, dd MMM yyyy HH:mm:ss 'GMT'", [Globalization.CultureInfo]::InvariantCulture)
    $writer.WriteElementString('pubDate', $date)
    $writer.WriteStartElement('enclosure')
    $writer.WriteAttributeString('url', $installer[0].browser_download_url)
    $writer.WriteAttributeString('length', [string]$installer[0].size)
    $writer.WriteAttributeString('type', 'application/octet-stream')
    $writer.WriteAttributeString('sparkle', 'os', $namespace, 'windows-x64')
    $writer.WriteAttributeString('sparkle', 'edSignature', $namespace, $signature)
    $writer.WriteEndElement()
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
if (-not $env:CLOUDFLARE_ACCOUNT_ID -or -not $env:CLOUDFLARE_API_TOKEN) {
    throw 'Set CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN for Pages deployment.'
}
if (-not (Get-Command wrangler -CommandType Application -ErrorAction SilentlyContinue)) {
    throw 'Install Wrangler and add it to PATH before deploying.'
}
& wrangler pages deploy $outputDirectory --project-name $ProjectName --branch main
if ($LASTEXITCODE -ne 0) { throw 'Cloudflare Pages deployment failed.' }
Write-Host "Published $feedUrl for $Tag."
