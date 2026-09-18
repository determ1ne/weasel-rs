#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Sign', 'Cleanup')][string]$Mode,
    [Parameter(Mandatory)][string]$CertificatePath,
    [string]$RendererPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$cert = $null
$keyDeleted = $false

# Only public certificates are retained. Never reuse an exportable signing key
# or a shared development certificate distributed with the installer.
function Remove-LocalTrust {
    param([string]$Receipt)
    if (-not (Test-Path -LiteralPath $Receipt -PathType Leaf)) { return }
    $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($Receipt)
    try {
        if ($cert.Subject -notmatch '^CN=Weasel-RS Local UIAccess [0-9a-f-]{36}$' -or $cert.Subject -ne $cert.Issuer) {
            throw 'Refusing to remove a certificate not issued by this installer.'
        }
        foreach ($store in @('My', 'TrustedPublisher', 'Root')) {
            $path = "Cert:\LocalMachine\$store\$($cert.Thumbprint)"
            if (Test-Path -LiteralPath $path) {
                if ($store -eq 'My') { Remove-Item -Path $path -DeleteKey -Force }
                else { Remove-Item -Path $path -Force }
            }
        }
    } finally { $cert.Dispose() }
}

try {
    if ($Mode -eq 'Cleanup') {
        Remove-LocalTrust $CertificatePath
        exit 0
    }
    if (-not $RendererPath -or -not (Test-Path -LiteralPath $RendererPath -PathType Leaf)) {
        throw 'Missing staged renderer.'
    }
    if (Test-Path -LiteralPath $CertificatePath) { throw 'Certificate receipt already exists.' }
    try {
        $cert = New-SelfSignedCertificate -Type CodeSigningCert `
            -Subject "CN=Weasel-RS Local UIAccess $([Guid]::NewGuid())" `
            -CertStoreLocation Cert:\LocalMachine\My `
            -Provider 'Microsoft Software Key Storage Provider' -KeyExportPolicy NonExportable `
            -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 -NotAfter (Get-Date).AddYears(1)
        $null = Export-Certificate -Cert $cert -FilePath $CertificatePath
        $null = Import-Certificate -FilePath $CertificatePath -CertStoreLocation Cert:\LocalMachine\Root
        $null = Import-Certificate -FilePath $CertificatePath -CertStoreLocation Cert:\LocalMachine\TrustedPublisher
        $result = Set-AuthenticodeSignature -LiteralPath $RendererPath -Certificate $cert -HashAlgorithm SHA256
        if ($result.Status -ne 'Valid') { throw "Signing failed: $($result.StatusMessage)" }
    } finally {
        if ($null -ne $cert) {
            # DeleteKey is a Certificate-provider dynamic parameter (use -Path).
            Remove-Item -Path "Cert:\LocalMachine\My\$($cert.Thumbprint)" -DeleteKey -Force
            $keyDeleted = $true
        }
    }
    # Verification does not need the private key. Destroy it before doing any
    # further work, not on installer exit or when uninstalling.
    $result = Get-AuthenticodeSignature -LiteralPath $RendererPath
    if ($result.Status -ne 'Valid' -or $result.SignerCertificate.Thumbprint -ne $cert.Thumbprint) {
        throw 'Signed renderer verification failed.'
    }
    Write-Output 'Renderer signed and verified; private key deleted.'
} catch {
    Write-Output $_.Exception.Message
    if ($Mode -eq 'Sign') {
        if ($null -ne $cert -and -not $keyDeleted) {
            Write-Output "Private key deletion failed; certificate thumbprint: $($cert.Thumbprint). Installation must stop."
            # Do not silently fall back if key cleanup cannot be established,
            # including failures before the public receipt was written.
            try { Remove-LocalTrust $CertificatePath }
            catch { Write-Output $_.Exception.Message }
            exit 2
        }
        try { Remove-LocalTrust $CertificatePath }
        catch { Write-Output "Certificate cleanup failed: $($_.Exception.Message)" }
    }
    exit 1
}
