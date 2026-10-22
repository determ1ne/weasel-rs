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
$feedUri = [Uri]$feedUrl
$siteRootUrl = "$($feedUri.GetLeftPart([UriPartial]::Authority))/"
$releaseNotesUrl = [Uri]::new($feedUri, '/release-notes.html').AbsoluteUri
$headers = @{ 'User-Agent' = 'weasel-rs-appcast-publisher'; 'Accept' = 'application/vnd.github+json' }
$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/tags/$Tag" -Headers $headers
if ($release.draft -or $release.prerelease -or -not $release.published_at) {
    throw 'Appcast may only point to a published, non-prerelease GitHub Release.'
}
$latest = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/latest" -Headers $headers
if ($latest.tag_name -cne $Tag) { throw "The latest published release is $($latest.tag_name), not $Tag." }

function ConvertTo-HtmlText([AllowNull()][string]$Text) {
    [Net.WebUtility]::HtmlEncode($Text)
}

function Format-FileSize([long]$Size) {
    if ($Size -ge 1GB) { return '{0:N1} GB' -f ($Size / 1GB) }
    if ($Size -ge 1MB) { return '{0:N1} MB' -f ($Size / 1MB) }
    if ($Size -ge 1KB) { return '{0:N1} KB' -f ($Size / 1KB) }
    return "$Size B"
}

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

$downloadSpecs = @(
    @{ Architecture = 'x64'; Kind = '完整版'; Suffix = ''; Description = '包含 VC++ 运行库及调试符号，适合大多数用户。' },
    @{ Architecture = 'x64'; Kind = 'Mini 版'; Suffix = '-mini'; Description = '安装时下载 VC++ 运行库，不包含调试符号。' },
    @{ Architecture = 'arm64'; Kind = '完整版'; Suffix = ''; Description = '适用于 Windows on Arm，包含 VC++ 运行库及调试符号。' },
    @{ Architecture = 'arm64'; Kind = 'Mini 版'; Suffix = '-mini'; Description = '适用于 Windows on Arm，安装时下载 VC++ 运行库。' }
)
$downloads = foreach ($spec in $downloadSpecs) {
    $assetName = "Weasel-RS-$version-$($spec.Architecture)$($spec.Suffix)-setup.exe"
    $asset = @($release.assets | Where-Object { $_.name -ceq $assetName })
    if ($asset.Count -ne 1) { throw "Release must contain exactly one $assetName." }
    if ($asset[0].size -le 0 -or $asset[0].browser_download_url -notmatch '^https://') {
        throw "Installer asset metadata is invalid: $assetName"
    }
    [pscustomobject]@{
        Architecture = $spec.Architecture
        Kind = $spec.Kind
        Description = $spec.Description
        Asset = $asset[0]
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
$generatedFiles = @('appcast.xml', 'index.html', 'release-notes.html', '_headers')
$otherFiles = @(
    Get-ChildItem -LiteralPath $outputDirectory -Force |
        Where-Object { $_.Name -cnotin $generatedFiles }
)
if ($otherFiles.Count -gt 0) { throw 'Update site output directory contains unexpected files; refusing to publish them.' }
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
    $writer.WriteElementString('sparkle', 'releaseNotesLink', $namespace, $releaseNotesUrl)
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

$releaseBody = [string]$release.body
$releaseBodyHtml = if ([string]::IsNullOrWhiteSpace($releaseBody)) {
    '<p>本版本暂无补充说明。</p>'
} else {
    $markdownRequest = @{
        text = $releaseBody
        mode = 'gfm'
        context = $repository
    } | ConvertTo-Json
    [string](Invoke-RestMethod -Method Post -Uri 'https://api.github.com/markdown' -Headers $headers `
        -ContentType 'application/json; charset=utf-8' -Body $markdownRequest)
}

$releaseTitle = if ([string]::IsNullOrWhiteSpace([string]$release.name)) {
    "小狼毫RS $version"
} else {
    [string]$release.name
}
$releaseTitleHtml = ConvertTo-HtmlText $releaseTitle
$tagHtml = ConvertTo-HtmlText $Tag
$publishedAt = ([datetimeoffset]$release.published_at).ToUniversalTime().ToString("yyyy-MM-dd HH:mm 'UTC'")
$publishedAtHtml = ConvertTo-HtmlText $publishedAt
$githubReleaseUrlHtml = ConvertTo-HtmlText $release.html_url
$repositoryUrl = "https://github.com/$repository"
$repositoryUrlHtml = ConvertTo-HtmlText $repositoryUrl
$siteRootUrlHtml = ConvertTo-HtmlText $siteRootUrl

$releaseNotesPage = @"
<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>$releaseTitleHtml · 小狼毫RS</title>
  <style>
    :root { color-scheme: light dark; font-family: "Segoe UI", "Microsoft YaHei UI", sans-serif; }
    body { margin: 0; color: #202124; background: #f5f7fa; }
    main { width: min(920px, calc(100% - 32px)); margin: 0 auto; padding: 48px 0 64px; }
    header, section { padding: 28px 32px; border: 1px solid #dce1e8; border-radius: 16px; background: #fff; box-shadow: 0 8px 28px rgba(25, 35, 50, .06); }
    section { margin-top: 20px; }
    h1 { margin: 0 0 8px; font-size: clamp(28px, 5vw, 42px); }
    h2 { margin-top: 0; }
    .meta { margin: 0; color: #626b78; }
    .downloads { display: grid; gap: 12px; }
    .download-card { display: flex; align-items: center; justify-content: space-between; gap: 24px; padding: 18px 20px; border: 1px solid #dce1e8; border-radius: 12px; }
    .download-card h3, .download-card p { margin: 0 0 5px; }
    .download-card p, .download-card small { color: #626b78; }
    .download-button { flex: none; padding: 9px 16px; border-radius: 8px; color: #fff; background: #146cce; text-decoration: none; font-weight: 600; }
    .download-button:hover { background: #0c58ac; }
    .release-body { line-height: 1.7; overflow-wrap: anywhere; }
    .release-body img { max-width: 100%; height: auto; }
    .release-body pre { overflow: auto; padding: 16px; border-radius: 8px; background: #f0f2f5; }
    .release-body table { display: block; max-width: 100%; overflow: auto; border-collapse: collapse; }
    .release-body th, .release-body td { padding: 7px 12px; border: 1px solid #dce1e8; }
    footer { margin-top: 24px; text-align: center; color: #626b78; }
    footer a { color: inherit; }
    @media (max-width: 640px) {
      main { width: min(100% - 20px, 920px); padding-top: 20px; }
      header, section { padding: 20px; border-radius: 12px; }
      .download-card { align-items: stretch; flex-direction: column; gap: 14px; }
      .download-button { text-align: center; }
    }
    @media (prefers-color-scheme: dark) {
      body { color: #eef1f5; background: #11151b; }
      header, section { border-color: #343b46; background: #1b2028; box-shadow: none; }
      .meta, .download-card p, .download-card small, footer { color: #aeb7c4; }
      .download-card, .release-body th, .release-body td { border-color: #343b46; }
      .release-body pre { background: #11151b; }
      .download-button { background: #2484ed; }
      .download-button:hover { background: #4097f2; }
    }
  </style>
</head>
<body>
  <main>
    <header>
      <h1>$releaseTitleHtml</h1>
      <p class="meta">版本 $tagHtml · 发布于 $publishedAtHtml</p>
    </header>
    <section class="release-body">
      <h2>更新说明</h2>
$releaseBodyHtml
    </section>
    <footer>
      <a href="$githubReleaseUrlHtml">在 GitHub 查看本次发布</a>
      · <a href="$repositoryUrlHtml">项目主页</a>
      · <a href="$siteRootUrlHtml">站点首页</a>
    </footer>
  </main>
</body>
</html>
"@

$indexPage = @"
<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta http-equiv="refresh" content="0; url=$repositoryUrlHtml">
  <link rel="canonical" href="$repositoryUrlHtml">
  <title>小狼毫RS</title>
</head>
<body>
  <p>正在前往 <a href="$repositoryUrlHtml">小狼毫RS GitHub 项目主页</a>……</p>
</body>
</html>
"@

$cloudflareHeaders = @'
/*
  Content-Security-Policy: default-src 'none'; img-src https: data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'
  Referrer-Policy: no-referrer
  X-Content-Type-Options: nosniff
  X-Frame-Options: DENY
  Permissions-Policy: camera=(), microphone=(), geolocation=()

/appcast.xml
  Cache-Control: no-cache

/release-notes.html
  Cache-Control: no-cache
'@

$utf8WithoutBom = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText((Join-Path $outputDirectory 'release-notes.html'), $releaseNotesPage, $utf8WithoutBom)
[IO.File]::WriteAllText((Join-Path $outputDirectory 'index.html'), $indexPage, $utf8WithoutBom)
[IO.File]::WriteAllText((Join-Path $outputDirectory '_headers'), $cloudflareHeaders, $utf8WithoutBom)

if ($DryRun) {
    Write-Host "Validated and generated the update site in $outputDirectory (not deployed)."
    return
}
if (-not $ProjectName) { throw 'Set WINSPARKLE_PAGES_PROJECT or pass -ProjectName.' }
if (-not (Get-Command wrangler -CommandType Application -ErrorAction SilentlyContinue)) {
    throw 'Install Wrangler and add it to PATH before deploying.'
}
& wrangler pages deploy $outputDirectory --project-name $ProjectName --branch main
if ($LASTEXITCODE -ne 0) { throw 'Cloudflare Pages deployment failed.' }
Write-Host "Published $feedUrl for $Tag."
